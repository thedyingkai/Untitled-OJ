use async_trait::async_trait;
use hmac::{Hmac, Mac};
use orchestrator_runtime::{
    AuthPipelineStep, GatewayPipelineStep, RuntimeMaterializationStep, TypedProvisionerStep,
};
use reqwest::{Client, Method, StatusCode, Url};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use time::OffsetDateTime;

const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 10_000;
const MAX_PIPELINE_CONFIG_BYTES: usize = 1024 * 1024;

const MANAGED_PIPELINE_ENVIRONMENT: &[&str] = &[
    "ORCHESTRATOR_SECRET_DIRECTORY",
    "ORCHESTRATOR_PIPELINE_PROVIDER_TIMEOUT_MS",
    "ORCHESTRATOR_REDIS_CONNECTIONS_JSON",
    "ORCHESTRATOR_REDIS_CONNECTIONS_FILE",
    "ORCHESTRATOR_STORAGE_CONNECTIONS_JSON",
    "ORCHESTRATOR_STORAGE_CONNECTIONS_FILE",
    "ORCHESTRATOR_FRONTEND_ASSET_STORES_JSON",
    "ORCHESTRATOR_FRONTEND_ASSET_STORES_FILE",
];

const LEGACY_PIPELINE_ENVIRONMENT: &[&str] = &[
    "OJOS_ENVIRONMENT",
    "ORCHESTRATOR_ENABLE_EXTERNAL_PROVISIONER_FALLBACK",
    "ORCHESTRATOR_AUTH_ADMIN_ENDPOINT",
    "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
    "ORCHESTRATOR_AUTH_ADMIN_TOKEN",
    "AUTH_SERVICE_ENDPOINT",
    "AUTH_SERVICE_ADMIN_TOKEN",
    "ORCHESTRATOR_GATEWAY_ADMIN_ENDPOINT",
    "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
    "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN",
    "GATEWAY_ENDPOINT",
    "GATEWAY_ADMIN_TOKEN",
    "ORCHESTRATOR_GATEWAY_TOKEN",
    "ORCHESTRATOR_RELEASE_PROVISIONER_ENDPOINT",
    "ORCHESTRATOR_RELEASE_PROVISIONER_ORIGIN",
    "ORCHESTRATOR_RELEASE_PROVISIONER_TOKEN",
];

const NODE_FORBIDDEN_MANAGEMENT_ENV: &[&str] = &[
    "ORCHESTRATOR_AUTH_ADMIN_ENDPOINT",
    "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
    "ORCHESTRATOR_AUTH_ADMIN_TOKEN",
    "AUTH_SERVICE_ENDPOINT",
    "AUTH_SERVICE_ADMIN_TOKEN",
    "ORCHESTRATOR_GATEWAY_ADMIN_ENDPOINT",
    "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
    "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN",
    "GATEWAY_ENDPOINT",
    "GATEWAY_ADMIN_TOKEN",
    "ORCHESTRATOR_GATEWAY_TOKEN",
    "ORCHESTRATOR_RELEASE_PROVISIONER_ENDPOINT",
    "ORCHESTRATOR_RELEASE_PROVISIONER_ORIGIN",
    "ORCHESTRATOR_RELEASE_PROVISIONER_TOKEN",
    "ORCHESTRATOR_API_REGISTRIES_JSON",
    "ORCHESTRATOR_API_REGISTRIES_FILE",
];

/// Defines which side of the trust boundary may execute release-management
/// providers. Production Node Agents are always `ManagedNode`: Auth, Gateway,
/// and generic management providers belong to the control plane and their
/// credentials must never enter the Agent process. The former external API
/// Registry provider is retired and rejected even in legacy mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PipelineProviderMode {
    #[default]
    ManagedNode,
    /// Compatibility escape hatch for the old local Compose workflow. The
    /// CLI exposes it only together with `OJOS_ENVIRONMENT=development`.
    LegacyDevelopment,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineProviderConfig {
    pub auth_endpoint: Option<String>,
    pub auth_admin_token: Option<String>,
    pub gateway_endpoint: Option<String>,
    pub gateway_admin_token: Option<String>,
    pub provisioner_endpoint: Option<String>,
    pub provisioner_token: Option<String>,
    pub secret_directory: Option<PathBuf>,
    pub timeout_ms: u64,
}

impl PipelineProviderConfig {
    /// Safe default for every managed Agent, including Desktop's loopback
    /// Agent. This constructor intentionally performs no management-secret
    /// environment lookups.
    pub fn managed_node() -> Self {
        Self {
            timeout_ms: DEFAULT_PROVIDER_TIMEOUT_MS,
            ..Self::default()
        }
    }

    /// Reads only Node-local materialization settings. Management endpoints
    /// and credentials are intentionally not part of this lookup surface.
    fn managed_node_from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        Self {
            secret_directory: first_value(&mut lookup, &["ORCHESTRATOR_SECRET_DIRECTORY"])
                .map(PathBuf::from),
            timeout_ms: lookup("ORCHESTRATOR_PIPELINE_PROVIDER_TIMEOUT_MS")
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(DEFAULT_PROVIDER_TIMEOUT_MS)
                .clamp(100, 60_000),
            ..Self::default()
        }
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        Self {
            auth_endpoint: first_value(
                &mut lookup,
                &[
                    "ORCHESTRATOR_AUTH_ADMIN_ENDPOINT",
                    "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
                    "AUTH_SERVICE_ENDPOINT",
                ],
            ),
            auth_admin_token: first_value(
                &mut lookup,
                &["ORCHESTRATOR_AUTH_ADMIN_TOKEN", "AUTH_SERVICE_ADMIN_TOKEN"],
            ),
            gateway_endpoint: first_value(
                &mut lookup,
                &[
                    "ORCHESTRATOR_GATEWAY_ADMIN_ENDPOINT",
                    "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
                    "GATEWAY_ENDPOINT",
                ],
            ),
            gateway_admin_token: first_value(
                &mut lookup,
                &[
                    "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN",
                    "GATEWAY_ADMIN_TOKEN",
                    "ORCHESTRATOR_GATEWAY_TOKEN",
                ],
            ),
            provisioner_endpoint: first_value(
                &mut lookup,
                &[
                    "ORCHESTRATOR_RELEASE_PROVISIONER_ENDPOINT",
                    "ORCHESTRATOR_RELEASE_PROVISIONER_ORIGIN",
                ],
            ),
            provisioner_token: first_value(
                &mut lookup,
                &["ORCHESTRATOR_RELEASE_PROVISIONER_TOKEN"],
            ),
            secret_directory: first_value(&mut lookup, &["ORCHESTRATOR_SECRET_DIRECTORY"])
                .map(PathBuf::from),
            timeout_ms: lookup("ORCHESTRATOR_PIPELINE_PROVIDER_TIMEOUT_MS")
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(DEFAULT_PROVIDER_TIMEOUT_MS)
                .clamp(100, 60_000),
        }
    }

    pub fn auth_configured(&self) -> bool {
        self.auth_endpoint.is_some() && self.auth_admin_token.is_some()
    }

    pub fn gateway_configured(&self) -> bool {
        self.gateway_endpoint.is_some() && self.gateway_admin_token.is_some()
    }

    pub fn provisioner_configured(&self) -> bool {
        self.provisioner_endpoint.is_some() && self.provisioner_token.is_some()
    }
}

fn first_value(lookup: &mut impl FnMut(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        lookup(name)
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
    })
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedisConnectionConfig {
    pub url: String,
}

/// Loads only the Agent-local Redis connection map needed for event context
/// materialization. Callers receive URLs in-process; only identifier keys are
/// ever published in runtime facts or persisted by the control plane.
pub fn event_connection_urls_from_env() -> Result<BTreeMap<String, String>, PipelineProviderError> {
    let values = snapshot_pipeline_environment(&[
        "ORCHESTRATOR_REDIS_CONNECTIONS_JSON",
        "ORCHESTRATOR_REDIS_CONNECTIONS_FILE",
    ])?;
    let configured: BTreeMap<String, RedisConnectionConfig> = json_env_or_file_source_with_lookup(
        &mut |name| values.get(name).cloned(),
        "ORCHESTRATOR_REDIS_CONNECTIONS_JSON",
        "ORCHESTRATOR_REDIS_CONNECTIONS_FILE",
    )?
    .value
    .unwrap_or_default();
    validated_event_connection_urls(configured)
}

fn validated_event_connection_urls(
    configured: BTreeMap<String, RedisConnectionConfig>,
) -> Result<BTreeMap<String, String>, PipelineProviderError> {
    if configured.len() > 64 {
        return Err(PipelineProviderError::Configuration(
            "Agent-local Redis connection map exceeds 64 entries".to_string(),
        ));
    }
    let mut result = BTreeMap::new();
    for (id, connection) in configured {
        let valid_id = !id.is_empty()
            && id.len() <= 128
            && id.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric()
                    || (index > 0 && matches!(byte, b'.' | b'_' | b':' | b'-'))
            });
        if !valid_id {
            return Err(PipelineProviderError::Configuration(
                "Agent-local Redis connection ID is not a stable identifier".to_string(),
            ));
        }
        if connection.url.is_empty()
            || connection.url.len() > 64 * 1024
            || connection.url.chars().any(char::is_whitespace)
            || !(connection.url.starts_with("redis://") || connection.url.starts_with("rediss://"))
            || redis::Client::open(connection.url.as_str()).is_err()
        {
            return Err(PipelineProviderError::Configuration(format!(
                "Agent-local Redis connection {id} is not a valid Redis URL"
            )));
        }
        result.insert(id, connection.url);
    }
    Ok(result)
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "backend", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageConnectionConfig {
    NodeDirectory {
        root: PathBuf,
    },
    S3 {
        endpoint: String,
        access_key: String,
        secret_key: String,
        #[serde(default = "default_s3_region")]
        region: String,
        #[serde(default = "default_true")]
        path_style: bool,
    },
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrontendAssetStoreConfig {
    pub root: PathBuf,
}

/// A single immutable view of every Agent-local pipeline setting. Production
/// startup captures environment variables and file-backed JSON exactly once;
/// both the workload-export boundary and the release provider are then built
/// from this object, removing any validate-then-reload window.
#[derive(Clone)]
pub struct PipelineBootstrapConfig {
    http: PipelineProviderConfig,
    redis_connections: BTreeMap<String, RedisConnectionConfig>,
    storage_connections: BTreeMap<String, StorageConnectionConfig>,
    frontend_asset_stores: BTreeMap<String, FrontendAssetStoreConfig>,
    event_connection_urls: BTreeMap<String, String>,
    internal_state_roots: Vec<PathBuf>,
    allow_external_provisioner_fallback: bool,
    mode: PipelineProviderMode,
}

impl PipelineBootstrapConfig {
    /// Capture settings for a managed in-process Agent without inspecting
    /// colocated control-plane management variables (used by safe loopback
    /// callers).
    pub fn from_managed_env() -> Result<Self, PipelineProviderError> {
        Self::from_environment(PipelineProviderMode::ManagedNode)
    }

    /// Capture settings for a production remote Agent. Management variables
    /// are presence-checked and rejected without reading their values.
    pub fn from_remote_agent_env() -> Result<Self, PipelineProviderError> {
        reject_node_management_environment()?;
        Self::from_environment(PipelineProviderMode::ManagedNode)
    }

    /// Capture the explicitly development-only legacy provider settings.
    pub fn from_legacy_development_env() -> Result<Self, PipelineProviderError> {
        Self::from_environment(PipelineProviderMode::LegacyDevelopment)
    }

    fn from_environment(mode: PipelineProviderMode) -> Result<Self, PipelineProviderError> {
        let mut values = BTreeMap::new();
        if mode == PipelineProviderMode::LegacyDevelopment {
            // Preserve the legacy trust gate: classify the environment before
            // reading any management credential value.
            values.extend(snapshot_pipeline_environment(&["OJOS_ENVIRONMENT"])?);
            validate_legacy_development_environment(
                values
                    .get("OJOS_ENVIRONMENT")
                    .map(String::as_str)
                    .unwrap_or_default(),
            )?;
        }
        let names: Vec<_> = match mode {
            PipelineProviderMode::ManagedNode => MANAGED_PIPELINE_ENVIRONMENT.to_vec(),
            PipelineProviderMode::LegacyDevelopment => MANAGED_PIPELINE_ENVIRONMENT
                .iter()
                .chain(LEGACY_PIPELINE_ENVIRONMENT)
                .copied()
                .filter(|name| *name != "OJOS_ENVIRONMENT")
                .collect(),
        };
        values.extend(snapshot_pipeline_environment(&names)?);
        Self::from_lookup(mode, |name| values.get(name).cloned())
    }

    fn from_lookup(
        mode: PipelineProviderMode,
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, PipelineProviderError> {
        if mode == PipelineProviderMode::LegacyDevelopment {
            validate_legacy_development_environment(
                lookup("OJOS_ENVIRONMENT").as_deref().unwrap_or_default(),
            )?;
        }
        let http = match mode {
            PipelineProviderMode::ManagedNode => {
                PipelineProviderConfig::managed_node_from_lookup(&mut lookup)
            }
            PipelineProviderMode::LegacyDevelopment => {
                PipelineProviderConfig::from_lookup(&mut lookup)
            }
        };
        let redis = json_env_or_file_source_with_lookup(
            &mut lookup,
            "ORCHESTRATOR_REDIS_CONNECTIONS_JSON",
            "ORCHESTRATOR_REDIS_CONNECTIONS_FILE",
        )?;
        let storage = json_env_or_file_source_with_lookup(
            &mut lookup,
            "ORCHESTRATOR_STORAGE_CONNECTIONS_JSON",
            "ORCHESTRATOR_STORAGE_CONNECTIONS_FILE",
        )?;
        let frontend = json_env_or_file_source_with_lookup(
            &mut lookup,
            "ORCHESTRATOR_FRONTEND_ASSET_STORES_JSON",
            "ORCHESTRATOR_FRONTEND_ASSET_STORES_FILE",
        )?;
        let redis_connections: BTreeMap<String, RedisConnectionConfig> =
            redis.value.unwrap_or_default();
        let storage_connections: BTreeMap<String, StorageConnectionConfig> =
            storage.value.unwrap_or_default();
        let frontend_asset_stores: BTreeMap<String, FrontendAssetStoreConfig> =
            frontend.value.unwrap_or_default();
        let event_connection_urls = validated_event_connection_urls(redis_connections.clone())?;

        let mut internal_state_roots = Vec::new();
        if let Some(root) = http.secret_directory.as_ref() {
            internal_state_roots.push(root.clone());
        }
        internal_state_roots.extend(
            [redis.file_parent, storage.file_parent, frontend.file_parent]
                .into_iter()
                .flatten(),
        );
        internal_state_roots.extend(storage_connections.values().filter_map(|connection| {
            match connection {
                StorageConnectionConfig::NodeDirectory { root } => Some(root.clone()),
                StorageConnectionConfig::S3 { .. } => None,
            }
        }));
        internal_state_roots.extend(
            frontend_asset_stores
                .values()
                .map(|store| store.root.clone()),
        );
        internal_state_roots.sort();
        internal_state_roots.dedup();

        let allow_external_provisioner_fallback = mode == PipelineProviderMode::LegacyDevelopment
            && lookup("ORCHESTRATOR_ENABLE_EXTERNAL_PROVISIONER_FALLBACK").is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        Ok(Self {
            http,
            redis_connections,
            storage_connections,
            frontend_asset_stores,
            event_connection_urls,
            internal_state_roots,
            allow_external_provisioner_fallback,
            mode,
        })
    }

    pub fn internal_state_roots(&self) -> &[PathBuf] {
        &self.internal_state_roots
    }

    pub fn event_connection_urls(&self) -> &BTreeMap<String, String> {
        &self.event_connection_urls
    }

    fn provider_config(&self, state_database: impl Into<PathBuf>) -> BuiltInPipelineProviderConfig {
        BuiltInPipelineProviderConfig {
            state_database: state_database.into(),
            redis_connections: self.redis_connections.clone(),
            storage_connections: self.storage_connections.clone(),
            frontend_asset_stores: self.frontend_asset_stores.clone(),
            allow_external_provisioner_fallback: self.allow_external_provisioner_fallback,
            mode: self.mode,
        }
    }

    pub fn build_release_provider(
        &self,
        state_database: impl Into<PathBuf>,
    ) -> Result<BuiltInReleasePipelineProvider, PipelineProviderError> {
        BuiltInReleasePipelineProvider::new(self.http.clone(), self.provider_config(state_database))
    }
}

#[derive(Clone)]
pub struct BuiltInPipelineProviderConfig {
    pub state_database: PathBuf,
    pub redis_connections: BTreeMap<String, RedisConnectionConfig>,
    pub storage_connections: BTreeMap<String, StorageConnectionConfig>,
    pub frontend_asset_stores: BTreeMap<String, FrontendAssetStoreConfig>,
    pub allow_external_provisioner_fallback: bool,
    pub mode: PipelineProviderMode,
}

impl BuiltInPipelineProviderConfig {
    pub fn new(state_database: impl Into<PathBuf>) -> Self {
        Self {
            state_database: state_database.into(),
            redis_connections: BTreeMap::new(),
            storage_connections: BTreeMap::new(),
            frontend_asset_stores: BTreeMap::new(),
            allow_external_provisioner_fallback: false,
            mode: PipelineProviderMode::ManagedNode,
        }
    }

    pub fn from_env_with_state_database(
        state_database: impl Into<PathBuf>,
    ) -> Result<Self, PipelineProviderError> {
        Ok(PipelineBootstrapConfig::from_managed_env()?.provider_config(state_database))
    }
}

/// Return every Agent-local path whose contents or descendants must remain
/// outside the workload-visible export tree. The paths are derived from the
/// same environment surface consumed by the pipeline provider, preventing a
/// deployment from passing startup isolation and loading credentials later
/// from inside the daemon-visible tree.
pub fn pipeline_internal_state_roots_from_env() -> Result<Vec<PathBuf>, PipelineProviderError> {
    Ok(PipelineBootstrapConfig::from_managed_env()?
        .internal_state_roots
        .clone())
}

#[cfg(test)]
fn pipeline_internal_state_roots_from_lookup(
    lookup: impl FnMut(&str) -> Option<String>,
) -> Result<Vec<PathBuf>, PipelineProviderError> {
    Ok(
        PipelineBootstrapConfig::from_lookup(PipelineProviderMode::ManagedNode, lookup)?
            .internal_state_roots,
    )
}

fn reject_node_management_environment() -> Result<(), PipelineProviderError> {
    let configured =
        configured_node_management_environment(|name| std::env::var_os(name).is_some());
    if configured.is_empty() {
        Ok(())
    } else {
        Err(PipelineProviderError::Configuration(format!(
            "managed Node Agent forbids control-plane management environment variables: {}; remove them from the Agent service",
            configured.join(", ")
        )))
    }
}

fn configured_node_management_environment(
    mut is_present: impl FnMut(&str) -> bool,
) -> Vec<&'static str> {
    NODE_FORBIDDEN_MANAGEMENT_ENV
        .iter()
        .copied()
        .filter(|name| is_present(name))
        .collect()
}

fn validate_legacy_development_environment(environment: &str) -> Result<(), PipelineProviderError> {
    if environment.trim().eq_ignore_ascii_case("development") {
        Ok(())
    } else {
        Err(PipelineProviderError::Configuration(
            "legacy release providers require both --legacy-release-providers and OJOS_ENVIRONMENT=development"
                .to_string(),
        ))
    }
}

struct JsonConfigSource<T> {
    value: Option<T>,
    file_parent: Option<PathBuf>,
}

fn json_env_or_file_source_with_lookup<T: DeserializeOwned>(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    json_name: &str,
    file_name: &str,
) -> Result<JsonConfigSource<T>, PipelineProviderError> {
    let inline = lookup(json_name);
    let file = lookup(file_name)
        .map(|value| PathBuf::from(value.trim()))
        .filter(|value| !value.as_os_str().is_empty());
    if inline.is_some() && file.is_some() {
        return Err(PipelineProviderError::Configuration(format!(
            "{json_name} and {file_name} are mutually exclusive"
        )));
    }
    let (payload, file_parent) = if let Some(payload) = inline {
        ensure_bounded_config(json_name, payload.as_bytes())?;
        (Some(payload), None)
    } else if let Some(path) = file {
        let parent = validate_pipeline_config_file_path(file_name, &path)?;
        (
            Some(read_bounded_utf8_config(file_name, &path)?),
            Some(parent),
        )
    } else {
        (None, None)
    };
    let value = payload
        .map(|payload| {
            serde_json::from_str(&payload).map_err(|error| {
                PipelineProviderError::Configuration(format!(
                    "decode {json_name}/{file_name}: {error}"
                ))
            })
        })
        .transpose()?;
    Ok(JsonConfigSource { value, file_parent })
}

fn snapshot_pipeline_environment(
    names: &[&str],
) -> Result<BTreeMap<String, String>, PipelineProviderError> {
    let mut values = BTreeMap::new();
    for name in names {
        match std::env::var(name) {
            Ok(value) => {
                ensure_bounded_config(name, value.as_bytes())?;
                values.insert((*name).to_string(), value);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(PipelineProviderError::Configuration(format!(
                    "{name} must be valid UTF-8"
                )));
            }
        }
    }
    Ok(values)
}

fn ensure_bounded_config(name: &str, bytes: &[u8]) -> Result<(), PipelineProviderError> {
    if bytes.len() > MAX_PIPELINE_CONFIG_BYTES {
        Err(PipelineProviderError::Configuration(format!(
            "{name} exceeds the {MAX_PIPELINE_CONFIG_BYTES}-byte configuration limit"
        )))
    } else {
        Ok(())
    }
}

fn validate_pipeline_config_file_path(
    name: &str,
    path: &Path,
) -> Result<PathBuf, PipelineProviderError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PipelineProviderError::Configuration(format!(
            "{name} must be an absolute normalized path"
        )));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "inspect {name} at {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PipelineProviderError::Configuration(format!(
            "{name} at {} must be a regular file, not a symlink",
            path.display()
        )));
    }
    path.parent()
        .filter(|parent| parent.is_absolute())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            PipelineProviderError::Configuration(format!(
                "{name} must have an absolute parent directory"
            ))
        })
}

fn read_bounded_utf8_config(name: &str, path: &Path) -> Result<String, PipelineProviderError> {
    let file = fs::File::open(path).map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "read {name} from {}: {error}",
            path.display()
        ))
    })?;
    let mut bytes = Vec::new();
    file.take((MAX_PIPELINE_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            PipelineProviderError::Configuration(format!(
                "read {name} from {}: {error}",
                path.display()
            ))
        })?;
    ensure_bounded_config(name, &bytes)?;
    String::from_utf8(bytes).map_err(|_| {
        PipelineProviderError::Configuration(format!(
            "{name} at {} must be valid UTF-8",
            path.display()
        ))
    })
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PipelineProviderError {
    #[error("pipeline provider configuration is invalid: {0}")]
    Configuration(String),
    #[error("pipeline provider {0} is not configured on this Node")]
    Unconfigured(&'static str),
    #[error(
        "pipeline provider {0} is control-plane-only for managed Service Contract v2 deployments"
    )]
    ControlPlaneOnly(&'static str),
    #[error("pipeline provider rejected the request with HTTP {status}: {body}")]
    Rejected { status: u16, body: String },
    #[error("pipeline provider request outcome is ambiguous: {0}")]
    Ambiguous(String),
}

impl PipelineProviderError {
    pub fn outcome_is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous(_))
    }
}

#[async_trait]
pub trait ReleasePipelineProvider: Send + Sync {
    async fn materialize_runtime(
        &self,
        step: &RuntimeMaterializationStep,
    ) -> Result<Vec<String>, PipelineProviderError>;
    async fn apply_auth(&self, step: &AuthPipelineStep) -> Result<(), PipelineProviderError>;
    async fn compensate_auth(&self, service_name: &str) -> Result<(), PipelineProviderError>;
    async fn restore_auth(
        &self,
        desired: Option<&AuthPipelineStep>,
        previous: Option<&AuthPipelineStep>,
    ) -> Result<(), PipelineProviderError> {
        if let Some(previous) = previous {
            self.apply_auth(previous).await
        } else if let Some(desired) = desired {
            self.compensate_auth(&desired.service_name).await
        } else {
            Ok(())
        }
    }
    async fn publish_gateway(
        &self,
        step: &GatewayPipelineStep,
    ) -> Result<(), PipelineProviderError>;
    async fn restore_gateway(
        &self,
        desired: Option<&GatewayPipelineStep>,
        previous: Option<&GatewayPipelineStep>,
        restore_revision_id: &str,
    ) -> Result<(), PipelineProviderError> {
        let mut restored =
            previous
                .or(desired)
                .cloned()
                .ok_or_else(|| PipelineProviderError::Rejected {
                    status: 422,
                    body: "Gateway restore requires a previous or desired state".to_string(),
                })?;
        restored.operation_id = restore_revision_id.to_string();
        if previous.is_none() {
            restored.routes.clear();
        }
        self.publish_gateway(&restored).await
    }
    async fn apply_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError>;
    async fn compensate_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError>;
    async fn restore_provisioner(
        &self,
        desired: Option<&TypedProvisionerStep>,
        previous: Option<&TypedProvisionerStep>,
    ) -> Result<(), PipelineProviderError> {
        if let Some(desired) = desired {
            self.compensate_provisioner(desired).await?;
        }
        if let Some(previous) = previous {
            self.apply_provisioner(previous).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct HttpReleasePipelineProvider {
    config: PipelineProviderConfig,
    client: Client,
}

#[derive(Clone)]
struct ProviderStateStore {
    path: PathBuf,
}

impl ProviderStateStore {
    fn open(path: &Path) -> Result<Self, PipelineProviderError> {
        let parent = path.parent().ok_or_else(|| {
            PipelineProviderError::Configuration(
                "provider state database requires a parent directory".to_string(),
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            PipelineProviderError::Configuration(format!(
                "create provider state directory {}: {error}",
                parent.display()
            ))
        })?;
        let store = Self {
            path: path.to_path_buf(),
        };
        let connection = store.connection()?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS provider_resources (
                    provider TEXT NOT NULL,
                    resource_key TEXT NOT NULL,
                    state_json TEXT NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    PRIMARY KEY(provider, resource_key)
                 );",
            )
            .map_err(provider_state_error)?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection, PipelineProviderError> {
        let connection = Connection::open(&self.path).map_err(provider_state_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(provider_state_error)?;
        Ok(connection)
    }

    fn put<T: Serialize>(
        &self,
        provider: &str,
        resource_key: &str,
        state: &T,
    ) -> Result<(), PipelineProviderError> {
        let payload =
            serde_json::to_string(state).map_err(|error| PipelineProviderError::Rejected {
                status: 500,
                body: format!("encode provider state: {error}"),
            })?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(provider_state_error)?;
        transaction
            .execute(
                "INSERT INTO provider_resources(provider, resource_key, state_json, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider, resource_key) DO UPDATE SET
                    state_json = excluded.state_json,
                    updated_at_ms = excluded.updated_at_ms",
                params![provider, resource_key, payload, crate::now_ms()],
            )
            .map_err(provider_state_error)?;
        transaction.commit().map_err(provider_state_error)
    }

    fn get(
        &self,
        provider: &str,
        resource_key: &str,
    ) -> Result<Option<Value>, PipelineProviderError> {
        let connection = self.connection()?;
        let payload = connection
            .query_row(
                "SELECT state_json FROM provider_resources WHERE provider = ?1 AND resource_key = ?2",
                params![provider, resource_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(provider_state_error)?;
        payload
            .map(|payload| {
                serde_json::from_str(&payload).map_err(|error| PipelineProviderError::Rejected {
                    status: 500,
                    body: format!("decode provider state: {error}"),
                })
            })
            .transpose()
    }

    fn delete(&self, provider: &str, resource_key: &str) -> Result<(), PipelineProviderError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(provider_state_error)?;
        transaction
            .execute(
                "DELETE FROM provider_resources WHERE provider = ?1 AND resource_key = ?2",
                params![provider, resource_key],
            )
            .map_err(provider_state_error)?;
        transaction.commit().map_err(provider_state_error)
    }
}

fn provider_state_error(error: rusqlite::Error) -> PipelineProviderError {
    PipelineProviderError::Rejected {
        status: 500,
        body: format!("provider state database failed: {error}"),
    }
}

#[derive(Clone)]
pub struct BuiltInReleasePipelineProvider {
    http: HttpReleasePipelineProvider,
    config: BuiltInPipelineProviderConfig,
    state: ProviderStateStore,
    client: Client,
}

impl BuiltInReleasePipelineProvider {
    pub fn new(
        http_config: PipelineProviderConfig,
        config: BuiltInPipelineProviderConfig,
    ) -> Result<Self, PipelineProviderError> {
        validate_builtin_config(&config)?;
        let state = ProviderStateStore::open(&config.state_database)?;
        let timeout_ms = if http_config.timeout_ms == 0 {
            DEFAULT_PROVIDER_TIMEOUT_MS
        } else {
            http_config.timeout_ms.clamp(100, 60_000)
        };
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                PipelineProviderError::Configuration(format!(
                    "build built-in provider HTTP client: {error}"
                ))
            })?;
        Ok(Self {
            http: HttpReleasePipelineProvider::new(http_config),
            config,
            state,
            client,
        })
    }

    pub fn from_env_with_state_database(
        state_database: impl Into<PathBuf>,
    ) -> Result<Self, PipelineProviderError> {
        PipelineBootstrapConfig::from_managed_env()?.build_release_provider(state_database)
    }

    /// Managed remote-Agent constructor. It refuses to start when deployment
    /// automation accidentally injects a control-plane management variable,
    /// while the Desktop loopback Agent may use the safe constructor above to
    /// ignore management variables owned by its colocated control plane.
    pub fn from_remote_agent_env_with_state_database(
        state_database: impl Into<PathBuf>,
    ) -> Result<Self, PipelineProviderError> {
        PipelineBootstrapConfig::from_remote_agent_env()?.build_release_provider(state_database)
    }

    /// Explicit compatibility constructor for old local Compose development.
    /// Production callers must use `from_env_with_state_database`.
    pub fn from_legacy_development_env_with_state_database(
        state_database: impl Into<PathBuf>,
    ) -> Result<Self, PipelineProviderError> {
        PipelineBootstrapConfig::from_legacy_development_env()?
            .build_release_provider(state_database)
    }
}

impl BuiltInReleasePipelineProvider {
    async fn ensure_s3_resource(
        &self,
        configured: &StorageConnectionConfig,
        service_name: &str,
        resource: &orchestrator_runtime::StorageResourceSpec,
    ) -> Result<(), PipelineProviderError> {
        let head = self
            .s3_request(configured, Method::HEAD, &resource.bucket, None, Vec::new())
            .await?;
        if head.status() == StatusCode::NOT_FOUND {
            let create = self
                .s3_request(configured, Method::PUT, &resource.bucket, None, Vec::new())
                .await?;
            if !create.status().is_success() && create.status() != StatusCode::CONFLICT {
                return Err(s3_rejected(create).await);
            }
        } else if !head.status().is_success() {
            return Err(s3_rejected(head).await);
        }
        let marker = s3_marker_key(service_name, resource);
        let body = serde_json::to_vec(&json!({
            "schema_version": 1,
            "service_name": service_name,
            "bucket": resource.bucket,
            "prefix": resource.prefix,
            "object_type": resource.object_type,
        }))
        .map_err(|error| PipelineProviderError::Rejected {
            status: 500,
            body: format!("encode S3 marker: {error}"),
        })?;
        let response = self
            .s3_request(
                configured,
                Method::PUT,
                &resource.bucket,
                Some(&marker),
                body,
            )
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(s3_rejected(response).await)
        }
    }

    async fn delete_s3_marker(
        &self,
        configured: &StorageConnectionConfig,
        service_name: &str,
        resource: &orchestrator_runtime::StorageResourceSpec,
    ) -> Result<(), PipelineProviderError> {
        let marker = s3_marker_key(service_name, resource);
        let response = self
            .s3_request(
                configured,
                Method::DELETE,
                &resource.bucket,
                Some(&marker),
                Vec::new(),
            )
            .await?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(s3_rejected(response).await)
        }
    }

    async fn s3_request(
        &self,
        configured: &StorageConnectionConfig,
        method: Method,
        bucket: &str,
        key: Option<&str>,
        body: Vec<u8>,
    ) -> Result<reqwest::Response, PipelineProviderError> {
        let StorageConnectionConfig::S3 {
            endpoint,
            access_key,
            secret_key,
            region,
            path_style: true,
        } = configured
        else {
            return Err(PipelineProviderError::Rejected {
                status: 422,
                body: "S3 request requires a path-style S3 connection".to_string(),
            });
        };
        let mut url = Url::parse(endpoint).map_err(|error| PipelineProviderError::Rejected {
            status: 422,
            body: format!("parse S3 endpoint: {error}"),
        })?;
        let mut path = url.path().trim_end_matches('/').to_string();
        path.push('/');
        path.push_str(bucket);
        if let Some(key) = key {
            path.push('/');
            path.push_str(key.trim_start_matches('/'));
        }
        url.set_path(&path);
        let canonical_uri = url.path().to_string();
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        let now = OffsetDateTime::now_utc();
        let date = format!(
            "{:04}{:02}{:02}",
            now.year(),
            u8::from(now.month()),
            now.day()
        );
        let amz_date = format!(
            "{date}T{:02}{:02}{:02}Z",
            now.hour(),
            now.minute(),
            now.second()
        );
        let payload_hash = hex_sha256(&body);
        let canonical_headers =
            format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "{}\n{}\n\n{}\n{}\n{}",
            method.as_str(),
            canonical_uri,
            canonical_headers,
            signed_headers,
            payload_hash
        );
        let scope = format!("{date}/{region}/s3/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex_sha256(canonical_request.as_bytes())
        );
        let date_key = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), date.as_bytes())?;
        let region_key = hmac_sha256(&date_key, region.as_bytes())?;
        let service_key = hmac_sha256(&region_key, b"s3")?;
        let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
        let signature = hex_bytes(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        );
        self.client
            .request(method, url)
            .header("Host", host)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", amz_date)
            .header("Authorization", authorization)
            .body(body)
            .send()
            .await
            .map_err(|error| {
                PipelineProviderError::Ambiguous(format!("S3 request outcome: {error}"))
            })
    }
}

async fn s3_rejected(response: reqwest::Response) -> PipelineProviderError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    PipelineProviderError::Rejected {
        status,
        body: body.chars().take(1_024).collect(),
    }
}

fn s3_marker_key(
    service_name: &str,
    resource: &orchestrator_runtime::StorageResourceSpec,
) -> String {
    let prefix = resource.prefix.trim_matches('/');
    if prefix.is_empty() {
        format!(".ojos-provisioned/{service_name}.json")
    } else {
        format!("{prefix}/.ojos-provisioned/{service_name}.json")
    }
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<Vec<u8>, PipelineProviderError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|error| {
        PipelineProviderError::Configuration(format!("initialize S3 signing key: {error}"))
    })?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_sha256(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

fn hex_bytes(value: &[u8]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_builtin_config(
    config: &BuiltInPipelineProviderConfig,
) -> Result<(), PipelineProviderError> {
    for (id, connection) in &config.redis_connections {
        validate_provider_id("Redis connection", id)?;
        redis::Client::open(connection.url.as_str()).map_err(|error| {
            PipelineProviderError::Configuration(format!(
                "Redis connection {id} URL is invalid: {error}"
            ))
        })?;
    }
    for (id, connection) in &config.storage_connections {
        validate_provider_id("Storage connection", id)?;
        match connection {
            StorageConnectionConfig::NodeDirectory { root } => preflight_directory(root)?,
            StorageConnectionConfig::S3 {
                endpoint,
                access_key,
                secret_key,
                region,
                path_style,
            } => {
                validate_http_endpoint("S3", id, endpoint)?;
                if access_key.trim().is_empty()
                    || secret_key.is_empty()
                    || region.trim().is_empty()
                    || !path_style
                {
                    return Err(PipelineProviderError::Configuration(format!(
                        "S3 connection {id} requires access_key, secret_key, region, and path_style=true"
                    )));
                }
            }
        }
    }
    for (id, store) in &config.frontend_asset_stores {
        validate_provider_id("Frontend asset store", id)?;
        preflight_directory(&store.root)?;
    }
    Ok(())
}

fn validate_provider_id(kind: &str, value: &str) -> Result<(), PipelineProviderError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PipelineProviderError::Configuration(format!(
            "{kind} id {value:?} is invalid"
        )));
    }
    Ok(())
}

fn validate_http_endpoint(
    kind: &str,
    id: &str,
    endpoint: &str,
) -> Result<Url, PipelineProviderError> {
    let url = Url::parse(endpoint).map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "{kind} connection {id} endpoint is invalid: {error}"
        ))
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PipelineProviderError::Configuration(format!(
            "{kind} connection {id} endpoint must be an HTTP(S) origin without credentials, query, or fragment"
        )));
    }
    Ok(url)
}

fn preflight_directory(path: &Path) -> Result<(), PipelineProviderError> {
    fs::create_dir_all(path).map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "create provider directory {}: {error}",
            path.display()
        ))
    })?;
    let root = fs::canonicalize(path).map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "resolve provider directory {}: {error}",
            path.display()
        ))
    })?;
    let probe = root.join(format!(
        ".provider-write-probe-{}-{}",
        std::process::id(),
        crate::now_ms()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
        .map_err(|error| {
            PipelineProviderError::Configuration(format!(
                "provider directory {} is not writable: {error}",
                root.display()
            ))
        })?;
    file.write_all(b"provider-preflight").map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "write provider directory {}: {error}",
            root.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "sync provider directory {}: {error}",
            root.display()
        ))
    })?;
    drop(file);
    fs::remove_file(probe).map_err(|error| {
        PipelineProviderError::Configuration(format!(
            "clean provider directory {}: {error}",
            root.display()
        ))
    })
}

impl HttpReleasePipelineProvider {
    pub fn new(config: PipelineProviderConfig) -> Self {
        let timeout_ms = if config.timeout_ms == 0 {
            DEFAULT_PROVIDER_TIMEOUT_MS
        } else {
            config.timeout_ms.clamp(100, 60_000)
        };
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client configuration is valid");
        Self { config, client }
    }

    pub fn from_env() -> Self {
        Self::new(PipelineProviderConfig::managed_node())
    }

    async fn require_success(
        request: reqwest::RequestBuilder,
    ) -> Result<(), PipelineProviderError> {
        let response = request
            .send()
            .await
            .map_err(|error| PipelineProviderError::Ambiguous(error.to_string()))?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(rejected(status, body))
    }
}

#[async_trait]
impl ReleasePipelineProvider for BuiltInReleasePipelineProvider {
    async fn materialize_runtime(
        &self,
        step: &RuntimeMaterializationStep,
    ) -> Result<Vec<String>, PipelineProviderError> {
        self.http.materialize_runtime(step).await
    }

    async fn apply_auth(&self, step: &AuthPipelineStep) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode {
            return Err(PipelineProviderError::ControlPlaneOnly("auth"));
        }
        self.http.apply_auth(step).await
    }

    async fn compensate_auth(&self, service_name: &str) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode {
            return Err(PipelineProviderError::ControlPlaneOnly("auth"));
        }
        self.http.compensate_auth(service_name).await
    }

    async fn restore_auth(
        &self,
        desired: Option<&AuthPipelineStep>,
        previous: Option<&AuthPipelineStep>,
    ) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode {
            return Err(PipelineProviderError::ControlPlaneOnly("auth"));
        }
        self.http.restore_auth(desired, previous).await
    }

    async fn publish_gateway(
        &self,
        step: &GatewayPipelineStep,
    ) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode {
            return Err(PipelineProviderError::ControlPlaneOnly("gateway"));
        }
        self.http.publish_gateway(step).await
    }

    async fn restore_gateway(
        &self,
        desired: Option<&GatewayPipelineStep>,
        previous: Option<&GatewayPipelineStep>,
        restore_revision_id: &str,
    ) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode {
            return Err(PipelineProviderError::ControlPlaneOnly("gateway"));
        }
        self.http
            .restore_gateway(desired, previous, restore_revision_id)
            .await
    }

    async fn apply_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode
            && matches!(step, TypedProvisionerStep::ApiRegistry { .. })
        {
            return Err(PipelineProviderError::ControlPlaneOnly("api_registry"));
        }
        let result = self.apply_builtin_provisioner(step).await;
        if matches!(result, Err(PipelineProviderError::Unconfigured(_)))
            && self.config.allow_external_provisioner_fallback
            && self.http.config.provisioner_configured()
        {
            self.http.apply_provisioner(step).await
        } else {
            result
        }
    }

    async fn compensate_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
        if self.config.mode == PipelineProviderMode::ManagedNode
            && matches!(step, TypedProvisionerStep::ApiRegistry { .. })
        {
            return Err(PipelineProviderError::ControlPlaneOnly("api_registry"));
        }
        let result = self.compensate_builtin_provisioner(step).await;
        if matches!(result, Err(PipelineProviderError::Unconfigured(_)))
            && self.config.allow_external_provisioner_fallback
            && self.http.config.provisioner_configured()
        {
            self.http.compensate_provisioner(step).await
        } else {
            result
        }
    }

    async fn restore_provisioner(
        &self,
        desired: Option<&TypedProvisionerStep>,
        previous: Option<&TypedProvisionerStep>,
    ) -> Result<(), PipelineProviderError> {
        if desired
            .zip(previous)
            .is_some_and(|(desired, previous)| desired.provider_name() != previous.provider_name())
        {
            return Err(PipelineProviderError::Rejected {
                status: 422,
                body: "provider restore states must have the same provider type".to_string(),
            });
        }
        let reference = previous
            .or(desired)
            .ok_or_else(|| PipelineProviderError::Rejected {
                status: 422,
                body: "provider restore requires a desired or previous state".to_string(),
            })?;
        if self.config.mode == PipelineProviderMode::ManagedNode
            && matches!(reference, TypedProvisionerStep::ApiRegistry { .. })
        {
            return Err(PipelineProviderError::ControlPlaneOnly("api_registry"));
        }
        if matches!(reference, TypedProvisionerStep::ApiRegistry { .. }) {
            return Err(PipelineProviderError::ControlPlaneOnly("api_registry"));
        }
        // Built-in Redis namespaces and Storage paths are durable resources;
        // restoring a revision reconciles the old declaration in place instead
        // of destructively deleting data first. Frontend publication similarly
        // rewrites the atomic current pointer to the prior signed descriptor.
        if let Some(previous) = previous {
            return self.apply_provisioner(previous).await;
        }
        if let Some(desired) = desired {
            return self.compensate_provisioner(desired).await;
        }
        Ok(())
    }
}

impl BuiltInReleasePipelineProvider {
    async fn apply_builtin_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
        match step {
            TypedProvisionerStep::Redis {
                service_name,
                resources,
            } => self.apply_redis(service_name, resources).await,
            TypedProvisionerStep::Storage {
                service_name,
                resources,
            } => self.apply_storage(service_name, resources).await,
            TypedProvisionerStep::Frontend {
                service_name,
                asset_store_id,
                version,
                route_prefix,
                remote_entry,
                metadata_source_url,
                metadata_sha256,
            } => self.apply_frontend(
                service_name,
                asset_store_id,
                version,
                route_prefix,
                remote_entry,
                metadata_source_url,
                metadata_sha256,
            ),
            TypedProvisionerStep::ApiRegistry { .. } => {
                Err(PipelineProviderError::ControlPlaneOnly("api_registry"))
            }
        }
    }

    async fn compensate_builtin_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
        match step {
            TypedProvisionerStep::Redis {
                service_name,
                resources,
            } => self.compensate_redis(service_name, resources),
            TypedProvisionerStep::Storage {
                service_name,
                resources,
            } => self.compensate_storage(service_name, resources).await,
            TypedProvisionerStep::Frontend {
                service_name,
                asset_store_id,
                version,
                metadata_sha256,
                ..
            } => self.compensate_frontend(service_name, asset_store_id, version, metadata_sha256),
            TypedProvisionerStep::ApiRegistry { .. } => {
                Err(PipelineProviderError::ControlPlaneOnly("api_registry"))
            }
        }
    }

    async fn apply_redis(
        &self,
        service_name: &str,
        resources: &[orchestrator_runtime::RedisNamespaceSpec],
    ) -> Result<(), PipelineProviderError> {
        let mut by_connection = BTreeMap::<&str, Vec<_>>::new();
        for resource in resources {
            validate_provider_id("Redis connection", &resource.connection_id)
                .map_err(configuration_as_rejection)?;
            validate_provider_resource_name("Redis resource", &resource.name)?;
            if resource.namespace.trim().is_empty() || resource.consumer_group.trim().is_empty() {
                return Err(PipelineProviderError::Rejected {
                    status: 422,
                    body: format!(
                        "Redis resource {} has an empty namespace/group",
                        resource.name
                    ),
                });
            }
            by_connection
                .entry(resource.connection_id.as_str())
                .or_default()
                .push(resource);
        }
        for (connection_id, resources) in by_connection {
            let configured = self
                .config
                .redis_connections
                .get(connection_id)
                .ok_or(PipelineProviderError::Unconfigured("redis"))?;
            let client = redis::Client::open(configured.url.as_str()).map_err(|error| {
                PipelineProviderError::Rejected {
                    status: 422,
                    body: format!("open Redis connection {connection_id}: {error}"),
                }
            })?;
            let mut connection =
                client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|error| {
                        PipelineProviderError::Ambiguous(format!(
                            "connect Redis {connection_id}: {error}"
                        ))
                    })?;
            let pong: String = redis::cmd("PING")
                .query_async(&mut connection)
                .await
                .map_err(|error| {
                    PipelineProviderError::Ambiguous(format!("ping Redis {connection_id}: {error}"))
                })?;
            if !pong.eq_ignore_ascii_case("PONG") {
                return Err(PipelineProviderError::Rejected {
                    status: 502,
                    body: format!("Redis {connection_id} returned unexpected PING value"),
                });
            }
            for resource in resources {
                if matches!(resource.kind.as_str(), "stream" | "consumer-group") {
                    let result: redis::RedisResult<String> = redis::cmd("XGROUP")
                        .arg("CREATE")
                        .arg(&resource.namespace)
                        .arg(&resource.consumer_group)
                        .arg("$")
                        .arg("MKSTREAM")
                        .query_async(&mut connection)
                        .await;
                    if let Err(error) = result
                        && !error.to_string().contains("BUSYGROUP")
                    {
                        return Err(PipelineProviderError::Ambiguous(format!(
                            "provision Redis resource {}: {error}",
                            resource.name
                        )));
                    }
                }
                self.state.put(
                    "redis",
                    &provider_resource_key(service_name, &resource.name),
                    &json!({
                        "service_name": service_name,
                        "connection_id": connection_id,
                        "resource": resource,
                    }),
                )?;
            }
        }
        Ok(())
    }

    fn compensate_redis(
        &self,
        service_name: &str,
        resources: &[orchestrator_runtime::RedisNamespaceSpec],
    ) -> Result<(), PipelineProviderError> {
        for resource in resources {
            // Redis namespaces are deterministic and can contain durable user
            // data. Compensation removes ownership registration but never
            // destroys a key that an old revision may still own.
            self.state.delete(
                "redis",
                &provider_resource_key(service_name, &resource.name),
            )?;
        }
        Ok(())
    }

    async fn apply_storage(
        &self,
        service_name: &str,
        resources: &[orchestrator_runtime::StorageResourceSpec],
    ) -> Result<(), PipelineProviderError> {
        for resource in resources {
            validate_provider_resource_name("Storage bucket", &resource.bucket)?;
            validate_relative_prefix(&resource.prefix)?;
            let configured = self
                .config
                .storage_connections
                .get(&resource.connection_id)
                .ok_or(PipelineProviderError::Unconfigured("storage"))?;
            match (resource.backend.as_str(), configured) {
                ("node_directory", StorageConnectionConfig::NodeDirectory { root }) => {
                    let canonical_root = fs::canonicalize(root).map_err(provider_io_error)?;
                    let path = canonical_root.join(&resource.bucket).join(&resource.prefix);
                    let existed_before = path.exists();
                    fs::create_dir_all(&path).map_err(provider_io_error)?;
                    let path = fs::canonicalize(path).map_err(provider_io_error)?;
                    if !path.starts_with(&canonical_root) {
                        return Err(PipelineProviderError::Rejected {
                            status: 422,
                            body: "Storage path escapes configured Node directory".to_string(),
                        });
                    }
                    if let Err(error) = self.state.put(
                        "storage",
                        &provider_resource_key(service_name, &storage_resource_key(resource)),
                        &json!({
                            "service_name": service_name,
                            "backend": "node_directory",
                            "connection_id": resource.connection_id,
                            "created": !existed_before,
                            "path": path,
                            "resource": resource,
                        }),
                    ) {
                        if !existed_before {
                            let _ = fs::remove_dir(&path);
                        }
                        return Err(error);
                    }
                }
                ("s3", configured @ StorageConnectionConfig::S3 { .. }) => {
                    self.ensure_s3_resource(configured, service_name, resource)
                        .await?;
                    self.state.put(
                        "storage",
                        &provider_resource_key(service_name, &storage_resource_key(resource)),
                        &json!({
                            "service_name": service_name,
                            "backend": "s3",
                            "connection_id": resource.connection_id,
                            "resource": resource,
                        }),
                    )?;
                }
                _ => {
                    return Err(PipelineProviderError::Rejected {
                        status: 422,
                        body: format!(
                            "Storage resource {} backend {} does not match connection {}",
                            resource.bucket, resource.backend, resource.connection_id
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    async fn compensate_storage(
        &self,
        service_name: &str,
        resources: &[orchestrator_runtime::StorageResourceSpec],
    ) -> Result<(), PipelineProviderError> {
        for resource in resources.iter().rev() {
            let state_key = provider_resource_key(service_name, &storage_resource_key(resource));
            let state = self.state.get("storage", &state_key)?;
            if state.is_none() && resource.backend == "node_directory" {
                // apply_storage cleans a newly-created empty directory itself
                // if its ownership row cannot commit. Without a row, deleting
                // a pre-existing operator directory would be unsafe.
                continue;
            }
            let backend = state
                .as_ref()
                .and_then(|state| state.get("backend"))
                .and_then(Value::as_str)
                .unwrap_or(resource.backend.as_str());
            match backend {
                "node_directory" => {
                    let created = state
                        .as_ref()
                        .and_then(|state| state.get("created"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !created {
                        self.state.delete("storage", &state_key)?;
                        continue;
                    }
                    let derived_path;
                    let path = if let Some(path) = state
                        .as_ref()
                        .and_then(|state| state.get("path"))
                        .and_then(Value::as_str)
                    {
                        Path::new(path)
                    } else {
                        let configured = self
                            .config
                            .storage_connections
                            .get(&resource.connection_id)
                            .ok_or(PipelineProviderError::Unconfigured("storage"))?;
                        let StorageConnectionConfig::NodeDirectory { root } = configured else {
                            return Err(PipelineProviderError::Rejected {
                                status: 422,
                                body: "Node storage compensation connection/backend mismatch"
                                    .to_string(),
                            });
                        };
                        derived_path = fs::canonicalize(root)
                            .map_err(provider_io_error)?
                            .join(&resource.bucket)
                            .join(&resource.prefix);
                        &derived_path
                    };
                    match fs::remove_dir(path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                            return Err(PipelineProviderError::Rejected {
                                status: 409,
                                body: format!(
                                    "Storage path {} contains data and requires operator reconciliation",
                                    path.display()
                                ),
                            });
                        }
                        Err(error) => return Err(provider_io_error(error)),
                    }
                }
                "s3" => {
                    let configured = self
                        .config
                        .storage_connections
                        .get(&resource.connection_id)
                        .ok_or(PipelineProviderError::Unconfigured("storage"))?;
                    self.delete_s3_marker(configured, service_name, resource)
                        .await?;
                }
                _ => {
                    return Err(PipelineProviderError::Rejected {
                        status: 500,
                        body: "Storage provider state has an unknown backend".to_string(),
                    });
                }
            }
            self.state.delete("storage", &state_key)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_frontend(
        &self,
        service_name: &str,
        asset_store_id: &str,
        version: &str,
        route_prefix: &str,
        remote_entry: &str,
        metadata_source_url: &str,
        metadata_sha256: &str,
    ) -> Result<(), PipelineProviderError> {
        let store = self
            .config
            .frontend_asset_stores
            .get(asset_store_id)
            .ok_or(PipelineProviderError::Unconfigured("frontend"))?;
        validate_provider_resource_name("Frontend service", service_name)?;
        validate_provider_resource_name("Frontend version", version)?;
        if !valid_sha256(metadata_sha256)
            || !route_prefix.starts_with('/')
            || !remote_entry.starts_with('/')
            || metadata_source_url.trim().is_empty()
        {
            return Err(PipelineProviderError::Rejected {
                status: 422,
                body:
                    "Frontend publication requires signed metadata and absolute route/entry paths"
                        .to_string(),
            });
        }
        let root = fs::canonicalize(&store.root).map_err(provider_io_error)?;
        let revision_directory = root.join("revisions").join(service_name);
        let current_directory = root.join("current");
        fs::create_dir_all(&revision_directory).map_err(provider_io_error)?;
        fs::create_dir_all(&current_directory).map_err(provider_io_error)?;
        let descriptor = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "service_name": service_name,
            "version": version,
            "route_prefix": route_prefix,
            "remote_entry": remote_entry,
            "metadata_source_url": metadata_source_url,
            "metadata_sha256": metadata_sha256,
        }))
        .map_err(|error| PipelineProviderError::Rejected {
            status: 500,
            body: format!("encode Frontend descriptor: {error}"),
        })?;
        let revision_path = revision_directory.join(format!(
            "{}-{}.json",
            version,
            metadata_sha256.trim_start_matches("sha256:")
        ));
        write_new_or_verify(&revision_path, &descriptor)?;
        let current_path = current_directory.join(format!("{service_name}.json"));
        atomic_replace(&current_path, &descriptor)?;
        self.state.put(
            "frontend",
            &provider_resource_key(service_name, asset_store_id),
            &json!({
                "asset_store_id": asset_store_id,
                "version": version,
                "metadata_sha256": metadata_sha256,
                "current_path": current_path,
                "revision_path": revision_path,
            }),
        )
    }

    fn compensate_frontend(
        &self,
        service_name: &str,
        asset_store_id: &str,
        version: &str,
        metadata_sha256: &str,
    ) -> Result<(), PipelineProviderError> {
        let store = self
            .config
            .frontend_asset_stores
            .get(asset_store_id)
            .ok_or(PipelineProviderError::Unconfigured("frontend"))?;
        let current_path = fs::canonicalize(&store.root)
            .map_err(provider_io_error)?
            .join("current")
            .join(format!("{service_name}.json"));
        if let Ok(bytes) = fs::read(&current_path)
            && let Ok(current) = serde_json::from_slice::<Value>(&bytes)
            && current.get("version").and_then(Value::as_str) == Some(version)
            && current.get("metadata_sha256").and_then(Value::as_str) == Some(metadata_sha256)
        {
            match fs::remove_file(&current_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(provider_io_error(error)),
            }
        }
        let key = provider_resource_key(service_name, asset_store_id);
        let Some(state) = self.state.get("frontend", &key)? else {
            return Ok(());
        };
        if state.get("version").and_then(Value::as_str) == Some(version)
            && state.get("metadata_sha256").and_then(Value::as_str) == Some(metadata_sha256)
        {
            self.state.delete("frontend", &key)?;
        }
        Ok(())
    }
}

fn rejected(status: StatusCode, body: String) -> PipelineProviderError {
    PipelineProviderError::Rejected {
        status: status.as_u16(),
        body: body.chars().take(1_024).collect(),
    }
}

#[async_trait]
impl ReleasePipelineProvider for HttpReleasePipelineProvider {
    async fn materialize_runtime(
        &self,
        step: &RuntimeMaterializationStep,
    ) -> Result<Vec<String>, PipelineProviderError> {
        let mut secrets = BTreeMap::new();
        if !step.secret_refs.is_empty() {
            let root = self.config.secret_directory.as_deref().ok_or(
                PipelineProviderError::Unconfigured("secret_materialization"),
            )?;
            let canonical_root =
                std::fs::canonicalize(root).map_err(|error| PipelineProviderError::Rejected {
                    status: 422,
                    body: format!("open configured secret directory: {error}"),
                })?;
            for (name, reference) in &step.secret_refs {
                if reference.is_empty()
                    || !reference.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
                {
                    return Err(PipelineProviderError::Rejected {
                        status: 422,
                        body: format!("secret reference {name} is invalid"),
                    });
                }
                let path = canonical_root.join(reference);
                let canonical = std::fs::canonicalize(&path).map_err(|error| {
                    PipelineProviderError::Rejected {
                        status: 422,
                        body: format!("resolve secret reference {name}: {error}"),
                    }
                })?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(PipelineProviderError::Rejected {
                        status: 422,
                        body: format!("secret reference {name} escapes configured directory"),
                    });
                }
                let value = std::fs::read_to_string(&canonical).map_err(|error| {
                    PipelineProviderError::Rejected {
                        status: 422,
                        body: format!("read secret reference {name}: {error}"),
                    }
                })?;
                if value.len() > 64 * 1024 || value.contains('\0') {
                    return Err(PipelineProviderError::Rejected {
                        status: 422,
                        body: format!("secret reference {name} is not a bounded text secret"),
                    });
                }
                secrets.insert(
                    name.clone(),
                    value.trim_end_matches(['\r', '\n']).to_string(),
                );
            }
        }
        step.environment_templates
            .iter()
            .map(|(key, template)| {
                expand_environment_template(template, &step.config, &secrets)
                    .map(|value| format!("{key}={value}"))
            })
            .collect()
    }

    async fn apply_auth(&self, step: &AuthPipelineStep) -> Result<(), PipelineProviderError> {
        let endpoint = self
            .config
            .auth_endpoint
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured("auth"))?;
        let token = self
            .config
            .auth_admin_token
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured("auth"))?;
        let permissions = step
            .permissions
            .iter()
            .map(|permission| {
                json!({
                    "code": permission,
                    "name": permission,
                    "description": format!("{} release permission", step.service_name),
                })
            })
            .collect::<Vec<_>>();
        let body = json!({
            "permissions": permissions,
            "default_role_bindings": [],
            "service_identity": step.service_identity,
        });
        let url = format!(
            "{}/auth/admin/services/{}/permissions",
            endpoint.trim_end_matches('/'),
            step.service_name
        );
        Self::require_success(self.client.post(url).bearer_auth(token).json(&body)).await
    }

    async fn compensate_auth(&self, service_name: &str) -> Result<(), PipelineProviderError> {
        let endpoint = self
            .config
            .auth_endpoint
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured("auth"))?;
        let token = self
            .config
            .auth_admin_token
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured("auth"))?;
        let url = format!(
            "{}/auth/admin/services/{}/permissions",
            endpoint.trim_end_matches('/'),
            service_name
        );
        Self::require_success(self.client.delete(url).bearer_auth(token)).await
    }

    async fn publish_gateway(
        &self,
        step: &GatewayPipelineStep,
    ) -> Result<(), PipelineProviderError> {
        let endpoint = self
            .config
            .gateway_endpoint
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured("gateway"))?;
        let token = self
            .config
            .gateway_admin_token
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured("gateway"))?;
        let routes = step
            .routes
            .iter()
            .map(|route| {
                json!({
                    "route_id": route.route_id,
                    "node_id": step.node_id,
                    "provider_node_id": step.node_id,
                    "provider_service_name": step.service_name,
                    "owner_service_id": step.service_name,
                    "prefix": route.path_prefix,
                    "service_id": step.service_name,
                    "target_service": step.service_name,
                    "upstream_base": route.upstream_base,
                    "auth_mode": route.auth_mode,
                    "required_permission": route.required_permission,
                    "methods": route.methods,
                    "enabled": true,
                    "proxy_enabled": true,
                    "priority": route.path_prefix.len(),
                    "created_from": "orchestrator_store_v1_pipeline",
                    "status": "active",
                    "service_status": "running",
                    "service_health": "ok",
                    "conflicts": [],
                    "warnings": [],
                    "blocked_by": [],
                })
            })
            .collect::<Vec<_>>();
        let body = json!({
            "operation_id": step.operation_id,
            "service_name": step.service_name,
            "node_id": step.node_id,
            "version": "1",
            "pushed_route_table": true,
            "routes": routes,
            "warnings": [],
            "can_proxy": !step.routes.is_empty(),
        });
        let url = format!(
            "{}/api/admin/orchestrator/routes/reload",
            endpoint.trim_end_matches('/')
        );
        Self::require_success(self.client.post(url).bearer_auth(token).json(&body)).await
    }

    async fn apply_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
        self.provisioner_request(step, "apply").await
    }

    async fn compensate_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
        self.provisioner_request(step, "compensate").await
    }

    async fn restore_provisioner(
        &self,
        desired: Option<&TypedProvisionerStep>,
        previous: Option<&TypedProvisionerStep>,
    ) -> Result<(), PipelineProviderError> {
        let provider = desired
            .or(previous)
            .map(TypedProvisionerStep::provider_name)
            .ok_or_else(|| PipelineProviderError::Rejected {
                status: 422,
                body: "provider restore requires a desired or previous state".to_string(),
            })?;
        if desired
            .zip(previous)
            .is_some_and(|(desired, previous)| desired.provider_name() != previous.provider_name())
        {
            return Err(PipelineProviderError::Rejected {
                status: 422,
                body: "provider restore states must have the same provider type".to_string(),
            });
        }
        let endpoint = self
            .config
            .provisioner_endpoint
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured(provider))?;
        let token = self
            .config
            .provisioner_token
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured(provider))?;
        let url = format!(
            "{}/api/v1/providers/{}:restore",
            endpoint.trim_end_matches('/'),
            provider
        );
        Self::require_success(self.client.post(url).bearer_auth(token).json(&json!({
            "desired": desired,
            "previous": previous,
        })))
        .await
    }
}

impl HttpReleasePipelineProvider {
    async fn provisioner_request(
        &self,
        step: &TypedProvisionerStep,
        action: &str,
    ) -> Result<(), PipelineProviderError> {
        let endpoint = self
            .config
            .provisioner_endpoint
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured(step.provider_name()))?;
        let token = self
            .config
            .provisioner_token
            .as_deref()
            .ok_or(PipelineProviderError::Unconfigured(step.provider_name()))?;
        let url = format!(
            "{}/api/v1/providers/{}:{}",
            endpoint.trim_end_matches('/'),
            step.provider_name(),
            action
        );
        Self::require_success(self.client.post(url).bearer_auth(token).json(step)).await
    }
}

fn configuration_as_rejection(error: PipelineProviderError) -> PipelineProviderError {
    PipelineProviderError::Rejected {
        status: 422,
        body: error.to_string(),
    }
}

fn validate_provider_resource_name(kind: &str, value: &str) -> Result<(), PipelineProviderError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PipelineProviderError::Rejected {
            status: 422,
            body: format!("{kind} {value:?} is invalid"),
        });
    }
    Ok(())
}

fn validate_relative_prefix(value: &str) -> Result<(), PipelineProviderError> {
    if value.is_empty()
        || Path::new(value).is_absolute()
        || Path::new(value)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PipelineProviderError::Rejected {
            status: 422,
            body: format!("Storage prefix {value:?} must be a non-empty relative path"),
        });
    }
    Ok(())
}

fn provider_resource_key(service_name: &str, resource_name: &str) -> String {
    format!("{service_name}/{resource_name}")
}

fn storage_resource_key(resource: &orchestrator_runtime::StorageResourceSpec) -> String {
    format!(
        "{}:{}:{}:{}",
        resource.connection_id, resource.bucket, resource.prefix, resource.object_type
    )
}

fn provider_io_error(error: std::io::Error) -> PipelineProviderError {
    PipelineProviderError::Ambiguous(format!("built-in provider filesystem I/O failed: {error}"))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_new_or_verify(path: &Path, bytes: &[u8]) -> Result<(), PipelineProviderError> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes).map_err(provider_io_error)?;
            file.sync_all().map_err(provider_io_error)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(path).map_err(provider_io_error)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(PipelineProviderError::Rejected {
                    status: 409,
                    body: format!(
                        "immutable provider artifact {} already exists with different bytes",
                        path.display()
                    ),
                })
            }
        }
        Err(error) => Err(provider_io_error(error)),
    }
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), PipelineProviderError> {
    let parent = path
        .parent()
        .ok_or_else(|| PipelineProviderError::Rejected {
            status: 422,
            body: "atomic provider target requires a parent directory".to_string(),
        })?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("provider"),
        std::process::id(),
        crate::now_ms()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(provider_io_error)?;
    if let Err(error) = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(provider_io_error)
    {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(provider_io_error(error));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that stay
    // alive for the duration of the call.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn expand_environment_template(
    template: &str,
    config: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, String>,
) -> Result<String, PipelineProviderError> {
    let mut output = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("${") {
        output.push_str(&remaining[..start]);
        let after = &remaining[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| PipelineProviderError::Rejected {
                status: 422,
                body: format!("unterminated environment template {template:?}"),
            })?;
        let key = &after[..end];
        let value = if let Some(key) = key.strip_prefix("config.") {
            config.get(key)
        } else if let Some(key) = key.strip_prefix("secret.") {
            secrets.get(key)
        } else {
            config.get(key).or_else(|| secrets.get(key))
        }
        .ok_or_else(|| PipelineProviderError::Rejected {
            status: 422,
            body: format!("environment template references unresolved value {key}"),
        })?;
        output.push_str(value);
        remaining = &after[end + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_runtime::{RedisNamespaceSpec, StorageResourceSpec, TypedProvisionerStep};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn provider_config(directory: &tempfile::TempDir) -> BuiltInPipelineProviderConfig {
        BuiltInPipelineProviderConfig::new(directory.path().join("provider-state.sqlite3"))
    }

    fn provider(config: BuiltInPipelineProviderConfig) -> BuiltInReleasePipelineProvider {
        BuiltInReleasePipelineProvider::new(PipelineProviderConfig::default(), config).unwrap()
    }

    #[test]
    fn pipeline_internal_roots_cover_secret_files_and_local_store_roots() {
        let directory = tempfile::tempdir().unwrap();
        let config_dir = directory.path().join("private-config");
        fs::create_dir_all(&config_dir).unwrap();
        let storage_file = config_dir.join("storage.json");
        let frontend_file = config_dir.join("frontend.json");
        let storage_root = directory.path().join("storage-data");
        let frontend_root = directory.path().join("frontend-data");
        fs::write(
            &storage_file,
            serde_json::to_vec(&BTreeMap::from([(
                "local".to_string(),
                StorageConnectionConfig::NodeDirectory {
                    root: storage_root.clone(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &frontend_file,
            serde_json::to_vec(&BTreeMap::from([(
                "assets".to_string(),
                FrontendAssetStoreConfig {
                    root: frontend_root.clone(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();
        let secret_root = directory.path().join("secrets");
        let redis_file = config_dir.join("redis.json");
        fs::write(&redis_file, "{}").unwrap();
        let values = BTreeMap::from([
            (
                "ORCHESTRATOR_SECRET_DIRECTORY".to_string(),
                secret_root.display().to_string(),
            ),
            (
                "ORCHESTRATOR_REDIS_CONNECTIONS_FILE".to_string(),
                redis_file.display().to_string(),
            ),
            (
                "ORCHESTRATOR_STORAGE_CONNECTIONS_FILE".to_string(),
                storage_file.display().to_string(),
            ),
            (
                "ORCHESTRATOR_FRONTEND_ASSET_STORES_FILE".to_string(),
                frontend_file.display().to_string(),
            ),
        ]);
        let roots = pipeline_internal_state_roots_from_lookup(|name| values.get(name).cloned())
            .expect("pipeline paths");
        assert!(roots.contains(&secret_root));
        assert!(roots.contains(&config_dir));
        assert!(roots.contains(&storage_root));
        assert!(roots.contains(&frontend_root));

        for export in [
            secret_root.join("export"),
            config_dir.join("export"),
            storage_root.join("export"),
            frontend_root.join("export"),
        ] {
            assert!(crate::validate_isolated_workload_roots(&export, &roots).is_err());
        }
    }

    #[test]
    fn pipeline_bootstrap_snapshot_is_immutable_and_shared_by_roots_and_provider() {
        let directory = tempfile::tempdir().unwrap();
        let config_dir = directory.path().join("private-config");
        fs::create_dir_all(&config_dir).unwrap();
        let redis_file = config_dir.join("redis.json");
        let storage_file = config_dir.join("storage.json");
        let frontend_file = config_dir.join("frontend.json");
        let original_storage_root = directory.path().join("original-storage");
        let replacement_storage_root = directory.path().join("replacement-storage");
        let original_frontend_root = directory.path().join("original-frontend");
        let replacement_frontend_root = directory.path().join("replacement-frontend");
        fs::create_dir_all(&original_storage_root).unwrap();
        fs::create_dir_all(&original_frontend_root).unwrap();

        fs::write(
            &redis_file,
            serde_json::to_vec(&BTreeMap::from([(
                "events".to_string(),
                RedisConnectionConfig {
                    url: "rediss://original:secret@redis.internal:6380/1".to_string(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &storage_file,
            serde_json::to_vec(&BTreeMap::from([(
                "local".to_string(),
                StorageConnectionConfig::NodeDirectory {
                    root: original_storage_root.clone(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &frontend_file,
            serde_json::to_vec(&BTreeMap::from([(
                "assets".to_string(),
                FrontendAssetStoreConfig {
                    root: original_frontend_root.clone(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();
        let values = BTreeMap::from([
            (
                "ORCHESTRATOR_REDIS_CONNECTIONS_FILE".to_string(),
                redis_file.display().to_string(),
            ),
            (
                "ORCHESTRATOR_STORAGE_CONNECTIONS_FILE".to_string(),
                storage_file.display().to_string(),
            ),
            (
                "ORCHESTRATOR_FRONTEND_ASSET_STORES_FILE".to_string(),
                frontend_file.display().to_string(),
            ),
        ]);
        let snapshot =
            PipelineBootstrapConfig::from_lookup(PipelineProviderMode::ManagedNode, |name| {
                values.get(name).cloned()
            })
            .unwrap();

        fs::write(
            &redis_file,
            r#"{"events":{"url":"rediss://replacement:secret@redis.internal:6380/2"}}"#,
        )
        .unwrap();
        fs::write(
            &storage_file,
            serde_json::to_vec(&BTreeMap::from([(
                "local".to_string(),
                StorageConnectionConfig::NodeDirectory {
                    root: replacement_storage_root.clone(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &frontend_file,
            serde_json::to_vec(&BTreeMap::from([(
                "assets".to_string(),
                FrontendAssetStoreConfig {
                    root: replacement_frontend_root.clone(),
                },
            )]))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            snapshot.event_connection_urls()["events"],
            "rediss://original:secret@redis.internal:6380/1"
        );
        assert!(snapshot.internal_state_roots().contains(&config_dir));
        assert!(
            snapshot
                .internal_state_roots()
                .contains(&original_storage_root)
        );
        assert!(
            snapshot
                .internal_state_roots()
                .contains(&original_frontend_root)
        );
        assert!(
            !snapshot
                .internal_state_roots()
                .contains(&replacement_storage_root)
        );
        let provider = snapshot
            .build_release_provider(directory.path().join("provider-state.sqlite3"))
            .unwrap();
        assert!(matches!(
            provider.config.storage_connections.get("local"),
            Some(StorageConnectionConfig::NodeDirectory { root }) if root == &original_storage_root
        ));
        assert_eq!(
            provider.config.frontend_asset_stores["assets"].root,
            original_frontend_root
        );
        assert_eq!(
            provider.config.redis_connections["events"].url,
            snapshot.event_connection_urls()["events"]
        );
    }

    #[test]
    fn pipeline_bootstrap_rejects_oversized_and_non_utf8_files() {
        let directory = tempfile::tempdir().unwrap();
        let oversized = directory.path().join("oversized.json");
        fs::write(&oversized, vec![b' '; MAX_PIPELINE_CONFIG_BYTES + 1]).unwrap();
        let values = BTreeMap::from([(
            "ORCHESTRATOR_REDIS_CONNECTIONS_FILE".to_string(),
            oversized.display().to_string(),
        )]);
        let error =
            PipelineBootstrapConfig::from_lookup(PipelineProviderMode::ManagedNode, |name| {
                values.get(name).cloned()
            })
            .err()
            .expect("oversized configuration must be rejected");
        assert!(error.to_string().contains("exceeds"));

        let non_utf8 = directory.path().join("non-utf8.json");
        fs::write(&non_utf8, [0xff]).unwrap();
        let values = BTreeMap::from([(
            "ORCHESTRATOR_REDIS_CONNECTIONS_FILE".to_string(),
            non_utf8.display().to_string(),
        )]);
        let error =
            PipelineBootstrapConfig::from_lookup(PipelineProviderMode::ManagedNode, |name| {
                values.get(name).cloned()
            })
            .err()
            .expect("non-UTF-8 configuration must be rejected");
        assert!(error.to_string().contains("valid UTF-8"));
    }

    #[test]
    fn legacy_admin_origin_aliases_are_supported_and_endpoint_has_priority() {
        let values = BTreeMap::from([
            (
                "ORCHESTRATOR_AUTH_ADMIN_ENDPOINT".to_string(),
                "https://auth-endpoint.example/".to_string(),
            ),
            (
                "ORCHESTRATOR_AUTH_ADMIN_ORIGIN".to_string(),
                "https://auth-origin.example/".to_string(),
            ),
            (
                "ORCHESTRATOR_GATEWAY_ADMIN_ENDPOINT".to_string(),
                "https://gateway-endpoint.example/".to_string(),
            ),
            (
                "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN".to_string(),
                "https://gateway-origin.example/".to_string(),
            ),
        ]);
        let configured = PipelineProviderConfig::from_lookup(|name| values.get(name).cloned());
        assert_eq!(
            configured.auth_endpoint.as_deref(),
            Some("https://auth-endpoint.example")
        );
        assert_eq!(
            configured.gateway_endpoint.as_deref(),
            Some("https://gateway-endpoint.example")
        );

        let origin_only = PipelineProviderConfig::from_lookup(|name| {
            values
                .get(name)
                .filter(|_| !name.ends_with("_ENDPOINT"))
                .cloned()
        });
        assert_eq!(
            origin_only.auth_endpoint.as_deref(),
            Some("https://auth-origin.example")
        );
        assert_eq!(
            origin_only.gateway_endpoint.as_deref(),
            Some("https://gateway-origin.example")
        );
    }

    #[test]
    fn managed_node_detects_management_credentials_without_reading_their_values() {
        let present = BTreeMap::from([
            ("ORCHESTRATOR_AUTH_ADMIN_TOKEN", "must-not-be-read"),
            ("ORCHESTRATOR_API_REGISTRIES_FILE", "must-not-be-opened"),
            ("UNRELATED", "allowed"),
        ]);
        assert_eq!(
            configured_node_management_environment(|name| present.contains_key(name)),
            vec![
                "ORCHESTRATOR_AUTH_ADMIN_TOKEN",
                "ORCHESTRATOR_API_REGISTRIES_FILE"
            ]
        );
    }

    #[test]
    fn managed_node_reads_only_local_materialization_settings() {
        let values = BTreeMap::from([
            (
                "ORCHESTRATOR_SECRET_DIRECTORY".to_string(),
                " /var/lib/ojos-agent/secrets ".to_string(),
            ),
            (
                "ORCHESTRATOR_PIPELINE_PROVIDER_TIMEOUT_MS".to_string(),
                "7500".to_string(),
            ),
            (
                "ORCHESTRATOR_AUTH_ADMIN_TOKEN".to_string(),
                "must-not-be-read".to_string(),
            ),
        ]);
        let mut requested = Vec::new();
        let configured = PipelineProviderConfig::managed_node_from_lookup(|name| {
            requested.push(name.to_string());
            values.get(name).cloned()
        });

        assert_eq!(
            configured.secret_directory,
            Some(PathBuf::from("/var/lib/ojos-agent/secrets"))
        );
        assert_eq!(configured.timeout_ms, 7500);
        assert!(configured.auth_endpoint.is_none());
        assert!(configured.auth_admin_token.is_none());
        assert!(!requested.iter().any(|name| name.contains("ADMIN")));
    }

    #[test]
    fn event_connection_configuration_validates_locally_and_returns_only_explicit_ids() {
        let secret_url = "rediss://event-user:event-secret@redis.internal:6380/4";
        let resolved = validated_event_connection_urls(BTreeMap::from([(
            "shared-events".to_string(),
            RedisConnectionConfig {
                url: secret_url.to_string(),
            },
        )]))
        .unwrap();
        assert_eq!(
            resolved.keys().cloned().collect::<Vec<_>>(),
            vec!["shared-events".to_string()]
        );
        assert_eq!(resolved["shared-events"], secret_url);

        let invalid_id = validated_event_connection_urls(BTreeMap::from([(
            "redis://not-an-id".to_string(),
            RedisConnectionConfig {
                url: secret_url.to_string(),
            },
        )]))
        .unwrap_err();
        assert!(!invalid_id.to_string().contains("event-secret"));

        let invalid_url = validated_event_connection_urls(BTreeMap::from([(
            "shared-events".to_string(),
            RedisConnectionConfig {
                url: "not-a-redis-url:event-secret".to_string(),
            },
        )]))
        .unwrap_err();
        assert!(!invalid_url.to_string().contains("event-secret"));
    }

    #[test]
    fn legacy_management_mode_is_rejected_outside_explicit_development() {
        assert!(validate_legacy_development_environment("development").is_ok());
        for environment in ["", "production", "staging", "legacy"] {
            assert_eq!(
                validate_legacy_development_environment(environment).unwrap_err(),
                PipelineProviderError::Configuration(
                    "legacy release providers require both --legacy-release-providers and OJOS_ENVIRONMENT=development"
                        .to_string(),
                )
            );
        }
    }

    #[tokio::test]
    async fn managed_node_never_executes_control_plane_management_providers() {
        let directory = tempfile::tempdir().unwrap();
        let provider = provider(provider_config(&directory));
        let auth = AuthPipelineStep {
            service_name: "service-api".to_string(),
            permissions: vec![],
            service_identity: None,
        };
        assert_eq!(
            provider.apply_auth(&auth).await.unwrap_err(),
            PipelineProviderError::ControlPlaneOnly("auth")
        );

        let gateway = GatewayPipelineStep {
            operation_id: "operation-1".to_string(),
            service_name: "service-api".to_string(),
            node_id: "node-a".to_string(),
            routes: vec![],
        };
        assert_eq!(
            provider.publish_gateway(&gateway).await.unwrap_err(),
            PipelineProviderError::ControlPlaneOnly("gateway")
        );

        let registry = TypedProvisionerStep::ApiRegistry {
            service_name: "service-api".to_string(),
            registry_id: "registry-main".to_string(),
            apis: vec![],
            required_apis: vec![],
        };
        assert_eq!(
            provider.apply_provisioner(&registry).await.unwrap_err(),
            PipelineProviderError::ControlPlaneOnly("api_registry")
        );
    }

    #[tokio::test]
    async fn node_storage_and_frontend_use_durable_atomic_builtin_state() {
        let directory = tempfile::tempdir().unwrap();
        let storage_root = directory.path().join("storage");
        let frontend_root = directory.path().join("assets");
        let mut config = provider_config(&directory);
        config.storage_connections.insert(
            "node-files".to_string(),
            StorageConnectionConfig::NodeDirectory {
                root: storage_root.clone(),
            },
        );
        config.frontend_asset_stores.insert(
            "gateway-assets".to_string(),
            FrontendAssetStoreConfig {
                root: frontend_root.clone(),
            },
        );
        let provider = provider(config);

        let storage = TypedProvisionerStep::Storage {
            service_name: "documents-api".to_string(),
            resources: vec![StorageResourceSpec {
                object_type: "document".to_string(),
                bucket: "service-data".to_string(),
                prefix: "documents/active".to_string(),
                backend: "node_directory".to_string(),
                connection_id: "node-files".to_string(),
            }],
        };
        provider.apply_provisioner(&storage).await.unwrap();
        assert!(
            storage_root
                .join("service-data")
                .join("documents")
                .join("active")
                .is_dir()
        );
        provider.compensate_provisioner(&storage).await.unwrap();
        assert!(
            !storage_root
                .join("service-data")
                .join("documents")
                .join("active")
                .exists()
        );

        let preexisting = storage_root
            .join("service-data")
            .join("documents")
            .join("active");
        fs::create_dir_all(&preexisting).unwrap();
        provider.apply_provisioner(&storage).await.unwrap();
        provider.compensate_provisioner(&storage).await.unwrap();
        assert!(
            preexisting.is_dir(),
            "compensation must not delete an operator-owned directory"
        );

        let frontend = |version: &str, checksum_character: char| TypedProvisionerStep::Frontend {
            service_name: "documents-api".to_string(),
            asset_store_id: "gateway-assets".to_string(),
            version: version.to_string(),
            route_prefix: "/documents".to_string(),
            remote_entry: "/remote-entry.js".to_string(),
            metadata_source_url: "https://catalog.example/documents.release.yaml".to_string(),
            metadata_sha256: format!(
                "sha256:{}",
                std::iter::repeat_n(checksum_character, 64).collect::<String>()
            ),
        };
        let previous = frontend("1.0.0", '1');
        let desired = frontend("2.0.0", '2');
        provider.apply_provisioner(&previous).await.unwrap();
        provider.apply_provisioner(&desired).await.unwrap();
        provider
            .restore_provisioner(Some(&desired), Some(&previous))
            .await
            .unwrap();
        let current: Value = serde_json::from_slice(
            &fs::read(frontend_root.join("current").join("documents-api.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(current["version"], "1.0.0");
        assert_eq!(
            current["metadata_sha256"],
            format!("sha256:{}", "1".repeat(64))
        );
    }

    #[tokio::test]
    async fn generic_http_provisioner_is_not_an_implicit_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let http = PipelineProviderConfig {
            provisioner_endpoint: Some("http://127.0.0.1:9".to_string()),
            provisioner_token: Some("configured-but-not-enabled".to_string()),
            ..PipelineProviderConfig::default()
        };
        let provider = BuiltInReleasePipelineProvider::new(
            http,
            BuiltInPipelineProviderConfig::new(directory.path().join("state.sqlite3")),
        )
        .unwrap();
        let step = TypedProvisionerStep::Redis {
            service_name: "service-api".to_string(),
            resources: vec![RedisNamespaceSpec {
                name: "events".to_string(),
                kind: "stream".to_string(),
                connection_id: "default".to_string(),
                namespace: "ojos:service-api:events".to_string(),
                consumer_group: "ojos-service-api-events".to_string(),
            }],
        };
        assert_eq!(
            provider.apply_provisioner(&step).await.unwrap_err(),
            PipelineProviderError::Unconfigured("redis")
        );
    }

    #[tokio::test]
    async fn redis_builtin_checks_registered_connection_and_creates_stream_group() {
        let (redis_url, server) = spawn_redis_server();
        let directory = tempfile::tempdir().unwrap();
        let mut config = provider_config(&directory);
        config.redis_connections.insert(
            "cache-main".to_string(),
            RedisConnectionConfig { url: redis_url },
        );
        let provider = provider(config);
        let step = TypedProvisionerStep::Redis {
            service_name: "service-api".to_string(),
            resources: vec![RedisNamespaceSpec {
                name: "events".to_string(),
                kind: "stream".to_string(),
                connection_id: "cache-main".to_string(),
                namespace: "ojos:service-api:events".to_string(),
                consumer_group: "ojos-service-api-events".to_string(),
            }],
        };
        provider.apply_provisioner(&step).await.unwrap();
        let commands = server.join().unwrap();
        assert!(commands.iter().any(|command| command[0] == "PING"));
        assert!(commands.iter().any(|command| command[0] == "XGROUP"));
    }

    #[tokio::test]
    async fn s3_builtin_creates_bucket_prefix_marker_and_compensates_marker() {
        let (endpoint, server) = spawn_http_sequence(vec![404, 200, 200, 204]);
        let directory = tempfile::tempdir().unwrap();
        let mut config = provider_config(&directory);
        config.storage_connections.insert(
            "object-main".to_string(),
            StorageConnectionConfig::S3 {
                endpoint,
                access_key: "test-access".to_string(),
                secret_key: "test-secret".to_string(),
                region: "us-test-1".to_string(),
                path_style: true,
            },
        );
        let provider = provider(config);
        let step = TypedProvisionerStep::Storage {
            service_name: "service-api".to_string(),
            resources: vec![StorageResourceSpec {
                object_type: "document".to_string(),
                bucket: "service-data".to_string(),
                prefix: "service-api/documents".to_string(),
                backend: "s3".to_string(),
                connection_id: "object-main".to_string(),
            }],
        };
        provider.apply_provisioner(&step).await.unwrap();
        provider.compensate_provisioner(&step).await.unwrap();
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("HEAD /service-data HTTP/1.1"));
        assert!(requests[1].starts_with("PUT /service-data HTTP/1.1"));
        assert!(requests[2].starts_with(
            "PUT /service-data/service-api/documents/.ojos-provisioned/service-api.json HTTP/1.1"
        ));
        assert!(requests[3].starts_with(
            "DELETE /service-data/service-api/documents/.ojos-provisioned/service-api.json HTTP/1.1"
        ));
        assert!(requests.iter().all(|request| {
            request
                .to_ascii_lowercase()
                .contains("authorization: aws4-hmac-sha256")
        }));
    }

    #[tokio::test]
    async fn retired_api_registry_step_has_no_external_execution_path() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = provider_config(&directory);
        config.mode = PipelineProviderMode::LegacyDevelopment;
        let provider = provider(config);
        let step = TypedProvisionerStep::ApiRegistry {
            service_name: "service-api".to_string(),
            registry_id: "registry-main".to_string(),
            apis: vec![],
            required_apis: vec![],
        };
        assert_eq!(
            provider.apply_provisioner(&step).await.unwrap_err(),
            PipelineProviderError::ControlPlaneOnly("api_registry")
        );
        assert_eq!(
            provider.compensate_provisioner(&step).await.unwrap_err(),
            PipelineProviderError::ControlPlaneOnly("api_registry")
        );
        assert_eq!(
            provider
                .restore_provisioner(Some(&step), None)
                .await
                .unwrap_err(),
            PipelineProviderError::ControlPlaneOnly("api_registry")
        );
    }

    fn spawn_http_sequence(statuses: Vec<u16>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            statuses
                .into_iter()
                .map(|status| {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = read_http_request(&mut stream);
                    let reason = match status {
                        200 => "OK",
                        204 => "No Content",
                        404 => "Not Found",
                        _ => "Status",
                    };
                    write!(
                        stream,
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    request
                })
                .collect()
        });
        (format!("http://{address}"), server)
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "HTTP client closed before headers");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(position) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .or_else(|| line.strip_prefix("Content-Length:"))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "HTTP client closed before body");
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(bytes).unwrap()
    }

    fn spawn_redis_server() -> (String, thread::JoinHandle<Vec<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buffered = Vec::new();
            let mut commands = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                while let Some((command, consumed)) = parse_resp_command(&buffered) {
                    buffered.drain(..consumed);
                    let reply = if command
                        .first()
                        .is_some_and(|value| value.eq_ignore_ascii_case("PING"))
                    {
                        b"+PONG\r\n".as_slice()
                    } else {
                        b"+OK\r\n".as_slice()
                    };
                    stream.write_all(reply).unwrap();
                    commands.push(command);
                }
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => buffered.extend_from_slice(&chunk[..read]),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(error) => panic!("Redis test server read failed: {error}"),
                }
            }
            commands
        });
        (format!("redis://{address}/"), server)
    }

    fn parse_resp_command(bytes: &[u8]) -> Option<(Vec<String>, usize)> {
        if bytes.first().copied()? != b'*' {
            return None;
        }
        let (count, mut cursor) = resp_line(bytes, 1)?;
        let count = std::str::from_utf8(count).ok()?.parse::<usize>().ok()?;
        let mut command = Vec::with_capacity(count);
        for _ in 0..count {
            if bytes.get(cursor).copied()? != b'$' {
                return None;
            }
            let (length, next) = resp_line(bytes, cursor + 1)?;
            let length = std::str::from_utf8(length).ok()?.parse::<usize>().ok()?;
            cursor = next;
            let end = cursor.checked_add(length)?;
            if bytes.get(end..end + 2)? != b"\r\n" {
                return None;
            }
            command.push(String::from_utf8(bytes[cursor..end].to_vec()).ok()?);
            cursor = end + 2;
        }
        Some((command, cursor))
    }

    fn resp_line(bytes: &[u8], start: usize) -> Option<(&[u8], usize)> {
        let relative = bytes
            .get(start..)?
            .windows(2)
            .position(|value| value == b"\r\n")?;
        let end = start + relative;
        Some((&bytes[start..end], end + 2))
    }
}
