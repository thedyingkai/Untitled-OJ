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
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use time::OffsetDateTime;

const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 10_000;

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
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
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

fn first_env(names: &[&str]) -> Option<String> {
    first_value(&mut |name| std::env::var(name).ok(), names)
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedisConnectionConfig {
    pub url: String,
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

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiRegistryConnectionConfig {
    pub endpoint: String,
    pub token: String,
}

#[derive(Clone)]
pub struct BuiltInPipelineProviderConfig {
    pub state_database: PathBuf,
    pub redis_connections: BTreeMap<String, RedisConnectionConfig>,
    pub storage_connections: BTreeMap<String, StorageConnectionConfig>,
    pub frontend_asset_stores: BTreeMap<String, FrontendAssetStoreConfig>,
    pub api_registries: BTreeMap<String, ApiRegistryConnectionConfig>,
    pub allow_external_provisioner_fallback: bool,
}

impl BuiltInPipelineProviderConfig {
    pub fn new(state_database: impl Into<PathBuf>) -> Self {
        Self {
            state_database: state_database.into(),
            redis_connections: BTreeMap::new(),
            storage_connections: BTreeMap::new(),
            frontend_asset_stores: BTreeMap::new(),
            api_registries: BTreeMap::new(),
            allow_external_provisioner_fallback: false,
        }
    }

    pub fn from_env_with_state_database(
        state_database: impl Into<PathBuf>,
    ) -> Result<Self, PipelineProviderError> {
        let mut config = Self::new(state_database);
        config.redis_connections = json_env_or_file(
            "ORCHESTRATOR_REDIS_CONNECTIONS_JSON",
            "ORCHESTRATOR_REDIS_CONNECTIONS_FILE",
        )?
        .unwrap_or_default();
        config.storage_connections = json_env_or_file(
            "ORCHESTRATOR_STORAGE_CONNECTIONS_JSON",
            "ORCHESTRATOR_STORAGE_CONNECTIONS_FILE",
        )?
        .unwrap_or_default();
        config.frontend_asset_stores = json_env_or_file(
            "ORCHESTRATOR_FRONTEND_ASSET_STORES_JSON",
            "ORCHESTRATOR_FRONTEND_ASSET_STORES_FILE",
        )?
        .unwrap_or_default();
        config.api_registries = json_env_or_file(
            "ORCHESTRATOR_API_REGISTRIES_JSON",
            "ORCHESTRATOR_API_REGISTRIES_FILE",
        )?
        .unwrap_or_default();
        config.allow_external_provisioner_fallback =
            first_env(&["ORCHESTRATOR_ENABLE_EXTERNAL_PROVISIONER_FALLBACK"]).is_some_and(
                |value| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                },
            );
        Ok(config)
    }
}

fn json_env_or_file<T: DeserializeOwned>(
    json_name: &str,
    file_name: &str,
) -> Result<Option<T>, PipelineProviderError> {
    let inline = std::env::var(json_name).ok();
    let file = std::env::var(file_name)
        .ok()
        .map(|value| PathBuf::from(value.trim()))
        .filter(|value| !value.as_os_str().is_empty());
    if inline.is_some() && file.is_some() {
        return Err(PipelineProviderError::Configuration(format!(
            "{json_name} and {file_name} are mutually exclusive"
        )));
    }
    let payload = if let Some(payload) = inline {
        Some(payload)
    } else if let Some(path) = file {
        Some(fs::read_to_string(&path).map_err(|error| {
            PipelineProviderError::Configuration(format!(
                "read {} from {}: {error}",
                file_name,
                path.display()
            ))
        })?)
    } else {
        None
    };
    payload
        .map(|payload| {
            serde_json::from_str(&payload).map_err(|error| {
                PipelineProviderError::Configuration(format!(
                    "decode {json_name}/{file_name}: {error}"
                ))
            })
        })
        .transpose()
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PipelineProviderError {
    #[error("pipeline provider configuration is invalid: {0}")]
    Configuration(String),
    #[error("pipeline provider {0} is not configured on this Node")]
    Unconfigured(&'static str),
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
        Self::new(
            PipelineProviderConfig::from_env(),
            BuiltInPipelineProviderConfig::from_env_with_state_database(state_database)?,
        )
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
    for (id, registry) in &config.api_registries {
        validate_provider_id("API registry", id)?;
        validate_http_endpoint("API registry", id, &registry.endpoint)?;
        if registry.token.trim().is_empty() {
            return Err(PipelineProviderError::Configuration(format!(
                "API registry {id} requires a token"
            )));
        }
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
        Self::new(PipelineProviderConfig::from_env())
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
        self.http.apply_auth(step).await
    }

    async fn compensate_auth(&self, service_name: &str) -> Result<(), PipelineProviderError> {
        self.http.compensate_auth(service_name).await
    }

    async fn restore_auth(
        &self,
        desired: Option<&AuthPipelineStep>,
        previous: Option<&AuthPipelineStep>,
    ) -> Result<(), PipelineProviderError> {
        self.http.restore_auth(desired, previous).await
    }

    async fn publish_gateway(
        &self,
        step: &GatewayPipelineStep,
    ) -> Result<(), PipelineProviderError> {
        self.http.publish_gateway(step).await
    }

    async fn restore_gateway(
        &self,
        desired: Option<&GatewayPipelineStep>,
        previous: Option<&GatewayPipelineStep>,
        restore_revision_id: &str,
    ) -> Result<(), PipelineProviderError> {
        self.http
            .restore_gateway(desired, previous, restore_revision_id)
            .await
    }

    async fn apply_provisioner(
        &self,
        step: &TypedProvisionerStep,
    ) -> Result<(), PipelineProviderError> {
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
        if matches!(reference, TypedProvisionerStep::ApiRegistry { .. }) {
            let result = self.restore_api_registry(desired, previous).await;
            if matches!(result, Err(PipelineProviderError::Unconfigured(_)))
                && self.config.allow_external_provisioner_fallback
                && self.http.config.provisioner_configured()
            {
                return self.http.restore_provisioner(desired, previous).await;
            }
            return result;
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
                self.api_registry_request("apply", Some(step), None).await
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
                self.api_registry_request("remove", Some(step), None).await
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

    async fn restore_api_registry(
        &self,
        desired: Option<&TypedProvisionerStep>,
        previous: Option<&TypedProvisionerStep>,
    ) -> Result<(), PipelineProviderError> {
        self.api_registry_request("restore", desired, previous)
            .await
    }

    async fn api_registry_request(
        &self,
        action: &str,
        desired: Option<&TypedProvisionerStep>,
        previous: Option<&TypedProvisionerStep>,
    ) -> Result<(), PipelineProviderError> {
        let reference = desired
            .or(previous)
            .ok_or_else(|| PipelineProviderError::Rejected {
                status: 422,
                body: "API registry request requires desired or previous state".to_string(),
            })?;
        let (service_name, registry_id) = match reference {
            TypedProvisionerStep::ApiRegistry {
                service_name,
                registry_id,
                ..
            } => (service_name, registry_id),
            _ => {
                return Err(PipelineProviderError::Rejected {
                    status: 422,
                    body: "API registry request received another provider type".to_string(),
                });
            }
        };
        let registry = self
            .config
            .api_registries
            .get(registry_id)
            .ok_or(PipelineProviderError::Unconfigured("api_registry"))?;
        let url = format!(
            "{}/api/v1/registry/releases:{action}",
            registry.endpoint.trim_end_matches('/')
        );
        let body = json!({
            "service_name": service_name,
            "registry_id": registry_id,
            "desired": desired,
            "previous": previous,
        });
        let idempotency = format!(
            "provider-{}",
            hex_sha256(serde_json::to_vec(&body).unwrap_or_default())
        );
        HttpReleasePipelineProvider::require_success(
            self.client
                .post(url)
                .bearer_auth(&registry.token)
                .header("Idempotency-Key", idempotency)
                .json(&body),
        )
        .await?;
        if action == "remove" {
            self.state.delete(
                "api_registry",
                &provider_resource_key(service_name, registry_id),
            )?;
        } else {
            self.state.put(
                "api_registry",
                &provider_resource_key(service_name, registry_id),
                &body,
            )?;
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
    use orchestrator_runtime::{
        ApiSurfaceSpec, RedisNamespaceSpec, StorageResourceSpec, TypedProvisionerStep,
    };
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
    fn admin_origin_aliases_are_supported_and_endpoint_has_priority() {
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
    async fn api_registry_uses_its_typed_control_interface() {
        let (endpoint, server) = spawn_http_sequence(vec![204, 204]);
        let directory = tempfile::tempdir().unwrap();
        let mut config = provider_config(&directory);
        config.api_registries.insert(
            "registry-main".to_string(),
            ApiRegistryConnectionConfig {
                endpoint,
                token: "registry-token".to_string(),
            },
        );
        let provider = provider(config);
        let step = TypedProvisionerStep::ApiRegistry {
            service_name: "service-api".to_string(),
            registry_id: "registry-main".to_string(),
            apis: vec![ApiSurfaceSpec {
                api_id: "service.read".to_string(),
                protocol: "http".to_string(),
                path_prefix: "/api".to_string(),
                methods: vec!["GET".to_string()],
                visibility: "global".to_string(),
                auth_mode: "user".to_string(),
                permission: "service.read".to_string(),
                version: "1".to_string(),
            }],
            required_apis: vec![],
        };
        provider.apply_provisioner(&step).await.unwrap();
        provider.compensate_provisioner(&step).await.unwrap();
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("POST /api/v1/registry/releases:apply HTTP/1.1"));
        assert!(requests[1].starts_with("POST /api/v1/registry/releases:remove HTTP/1.1"));
        assert!(requests.iter().all(|request| {
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer registry-token")
        }));
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
