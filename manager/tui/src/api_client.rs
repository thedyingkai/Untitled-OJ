//! Strict `/api/v1` client used by the remote TUI.
//!
//! The client deliberately keeps transport and product actions separate.  A
//! mutation is sent only when the control plane publishes the corresponding
//! capability, always carries an idempotency key, and preserves RFC problem
//! details instead of turning an HTTP response into a success message.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ureq::http::Uri;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
static IDEMPOTENCY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, PartialEq, Eq)]
pub struct ApiClientConfig {
    pub base_url: String,
    pub bearer_token: Option<String>,
    pub timeout: Duration,
}

impl ApiClientConfig {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ApiError> {
        let base_url = normalize_base_url(&base_url.into())?;
        Ok(Self {
            base_url,
            bearer_token: None,
            timeout: Duration::from_secs(15),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub trait ApiTransport: Send + Sync {
    fn execute(&self, request: TransportRequest) -> Result<TransportResponse, ApiError>;
}

#[derive(Clone)]
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new(timeout: Duration) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        Self { agent }
    }

    fn collect_response(
        response: ureq::http::Response<ureq::Body>,
    ) -> Result<TransportResponse, ApiError> {
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect::<BTreeMap<_, _>>();
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|err| ApiError::Transport(err.to_string()))?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(ApiError::InvalidResponse(format!(
                "response exceeds {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        Ok(TransportResponse {
            status,
            headers,
            body,
        })
    }
}

impl ApiTransport for UreqTransport {
    fn execute(&self, request: TransportRequest) -> Result<TransportResponse, ApiError> {
        let response = match request.method.as_str() {
            "GET" => apply_headers(self.agent.get(&request.url), &request.headers)
                .call()
                .map_err(|err| ApiError::Transport(err.to_string()))?,
            "POST" => apply_headers(self.agent.post(&request.url), &request.headers)
                .send(request.body)
                .map_err(|err| ApiError::Transport(err.to_string()))?,
            "PATCH" => apply_headers(self.agent.patch(&request.url), &request.headers)
                .send(request.body)
                .map_err(|err| ApiError::Transport(err.to_string()))?,
            "DELETE" => apply_headers(self.agent.delete(&request.url), &request.headers)
                .call()
                .map_err(|err| ApiError::Transport(err.to_string()))?,
            method => {
                return Err(ApiError::InvalidRequest(format!(
                    "unsupported method {method}"
                )));
            }
        };
        Self::collect_response(response)
    }
}

fn apply_headers<T>(
    mut builder: ureq::RequestBuilder<T>,
    headers: &BTreeMap<String, String>,
) -> ureq::RequestBuilder<T> {
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Capability {
    pub action: String,
    #[serde(default)]
    pub target_type: String,
    #[serde(default)]
    pub capability_status: String,
    #[serde(default)]
    pub required_permission: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    entries: BTreeMap<String, Capability>,
}

impl CapabilitySet {
    pub fn from_entries(entries: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .filter(|entry| {
                    !matches!(
                        entry.capability_status.to_ascii_uppercase().as_str(),
                        "UNSUPPORTED" | "FAILED" | "BLOCKED"
                    )
                })
                .map(|entry| (entry.action.clone(), entry))
                .collect(),
        }
    }

    pub fn supports(&self, action: &str) -> bool {
        self.entries.contains_key(action)
    }

    pub fn actions(&self) -> BTreeSet<String> {
        self.entries.keys().cloned().collect()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ResponseMeta {
    pub request_id: String,
    pub api_version: String,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Envelope {
    data: Value,
    meta: ResponseMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiSuccess {
    pub status: u16,
    pub data: Value,
    pub meta: ResponseMeta,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProblemDetails {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub title: String,
    pub status: u16,
    #[serde(default)]
    pub code: String,
    pub detail: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ApiError {
    #[error("invalid API configuration: {0}")]
    InvalidConfiguration(String),
    #[error("invalid API request: {0}")]
    InvalidRequest(String),
    #[error("API transport failed: {0}")]
    Transport(String),
    #[error("invalid API response: {0}")]
    InvalidResponse(String),
    #[error("capability {action} is not published by this control plane")]
    CapabilityUnavailable { action: String },
    #[error("{code}: {detail} (HTTP {status}, request {request_id})")]
    Problem {
        status: u16,
        code: String,
        detail: String,
        request_id: String,
        operation_id: Option<String>,
        retry_after: Option<String>,
    },
}

impl ApiError {
    pub fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Problem { retry_after, .. } => retry_after.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstallApiBindingSelection {
    pub name: String,
    pub provider_deployment_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstallTopologySelection {
    pub topology_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct StorePipelineOptions {
    pub start: bool,
    pub migration_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_node_id: Option<String>,
    pub config: Value,
    pub secret_refs: BTreeMap<String, String>,
}

impl Default for StorePipelineOptions {
    fn default() -> Self {
        Self {
            start: true,
            migration_policy: "APPLY".to_string(),
            gateway_node_id: None,
            config: json!({}),
            secret_refs: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StoreInstallInput {
    pub service_id: String,
    pub target_node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub mode: String,
    pub start: bool,
    pub migration_policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_node_id: Option<String>,
    pub config: Value,
    pub secret_refs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<InstallApiBindingSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_etag: Option<String>,
}

/// The complete, deliberately narrow body accepted by ResourceClaim purge.
/// Credentials, secret references and actor identity are not part of this
/// type, so the TUI cannot forward them even if they are supplied elsewhere.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResourcePurgeInput {
    pub node_id: String,
    pub claim_digest: String,
    pub generation: u64,
    pub confirmation: String,
    pub reason: String,
}

impl ResourcePurgeInput {
    fn validate(&self, claim_id: &str) -> Result<(), ApiError> {
        validate_resource_identifier(&self.node_id, "node_id")?;
        if !is_sha256_digest(&self.claim_digest) {
            return Err(ApiError::InvalidRequest(
                "resource purge claim_digest must be sha256 followed by 64 lowercase hexadecimal characters"
                    .to_string(),
            ));
        }
        if self.generation == 0 {
            return Err(ApiError::InvalidRequest(
                "resource purge generation must be at least 1".to_string(),
            ));
        }
        let expected = format!(
            "PURGE {claim_id} {} GENERATION {}",
            self.claim_digest, self.generation
        );
        if self.confirmation != expected {
            return Err(ApiError::InvalidRequest(format!(
                "resource purge confirmation must exactly equal {expected:?}"
            )));
        }
        let reason_length = self.reason.chars().count();
        if self.reason.trim().chars().count() < 8 || reason_length > 512 {
            return Err(ApiError::InvalidRequest(
                "resource purge reason must contain 8 to 512 characters".to_string(),
            ));
        }
        Ok(())
    }
}

impl StoreInstallInput {
    pub fn managed(service_id: impl Into<String>, target_node_id: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            target_node_id: target_node_id.into(),
            catalog_source_id: None,
            version: None,
            channel: "stable".to_string(),
            endpoint: None,
            mode: "MANAGED".to_string(),
            start: true,
            migration_policy: "APPLY".to_string(),
            gateway_node_id: None,
            config: json!({}),
            secret_refs: BTreeMap::new(),
            bindings: Vec::new(),
            topology_id: None,
            topology_etag: None,
        }
    }

    pub fn apply_pipeline_options(&mut self, options: &StorePipelineOptions) {
        self.start = options.start;
        self.migration_policy.clone_from(&options.migration_policy);
        self.gateway_node_id.clone_from(&options.gateway_node_id);
        self.config.clone_from(&options.config);
        self.secret_refs.clone_from(&options.secret_refs);
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogSourceInput {
    pub id: String,
    pub url: String,
    pub required_key_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_secret_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub offline_oci_layouts: BTreeMap<String, String>,
}

impl CatalogSourceInput {
    pub fn trusted(
        id: impl Into<String>,
        url: impl Into<String>,
        required_key_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            required_key_id: required_key_id.into(),
            auth_secret_ref: String::new(),
            public_key: None,
            enabled: true,
            offline_oci_layouts: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorePackageQuery {
    pub search: Option<String>,
    pub channel: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub variant: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SseEvent {
    pub id: String,
    pub event: String,
    pub data: Value,
}

#[derive(Clone)]
pub struct ApiClient {
    config: ApiClientConfig,
    transport: Arc<dyn ApiTransport>,
    capabilities: Arc<Mutex<Option<CapabilitySet>>>,
}

impl ApiClient {
    pub fn connect(config: ApiClientConfig) -> Self {
        let transport = Arc::new(UreqTransport::new(config.timeout));
        Self::with_transport(config, transport)
    }

    pub fn with_transport(config: ApiClientConfig, transport: Arc<dyn ApiTransport>) -> Self {
        Self {
            config,
            transport,
            capabilities: Arc::new(Mutex::new(None)),
        }
    }

    pub fn capabilities(&self, refresh: bool) -> Result<CapabilitySet, ApiError> {
        if !refresh {
            let cached = self
                .capabilities
                .lock()
                .map_err(|_| ApiError::Transport("capability cache lock poisoned".to_string()))?
                .clone();
            if let Some(cached) = cached {
                return Ok(cached);
            }
        }
        let response = self.get("/capabilities", &[])?;
        let entries = response.data.get("actions").cloned().ok_or_else(|| {
            ApiError::InvalidResponse("capabilities.actions is missing".to_string())
        })?;
        let capabilities = CapabilitySet::from_entries(
            serde_json::from_value::<Vec<Capability>>(entries)
                .map_err(|err| ApiError::InvalidResponse(err.to_string()))?,
        );
        *self
            .capabilities
            .lock()
            .map_err(|_| ApiError::Transport("capability cache lock poisoned".to_string()))? =
            Some(capabilities.clone());
        Ok(capabilities)
    }

    pub fn list_catalogs(&self, cursor: Option<&str>) -> Result<ApiSuccess, ApiError> {
        self.read("catalog.list", "/store/catalogs", &cursor_query(cursor))
    }

    pub fn register_catalog(&self, source: CatalogSourceInput) -> Result<ApiSuccess, ApiError> {
        for (name, value) in [
            ("id", source.id.as_str()),
            ("url", source.url.as_str()),
            ("required_key_id", source.required_key_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ApiError::InvalidRequest(format!("{name} is required")));
            }
        }
        if let Some(public_key) = source.public_key.as_deref() {
            let decoded = BASE64_STANDARD.decode(public_key).map_err(|_| {
                ApiError::InvalidRequest("public_key must be canonical padded base64".to_string())
            })?;
            if decoded.len() != 32 || BASE64_STANDARD.encode(&decoded) != public_key {
                return Err(ApiError::InvalidRequest(
                    "public_key must be the canonical padded-base64 encoding of a raw 32-byte Ed25519 public key"
                        .to_string(),
                ));
            }
        }
        self.mutate(
            "catalog.register",
            "POST",
            "/store/catalogs",
            serde_json::to_value(source)
                .map_err(|err| ApiError::InvalidRequest(err.to_string()))?,
            &[],
        )
    }

    pub fn remove_catalog(&self, source_id: &str) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "catalog.remove",
            "DELETE",
            &format!("/store/catalogs/{}", path_segment(source_id)?),
            Value::Null,
            &[],
        )
    }

    pub fn search_store_packages(&self, query: &StorePackageQuery) -> Result<ApiSuccess, ApiError> {
        let mut values = cursor_query(query.cursor.as_deref());
        for (name, value) in [
            ("search", query.search.as_deref()),
            ("channel", query.channel.as_deref()),
            ("os", query.os.as_deref()),
            ("arch", query.arch.as_deref()),
            ("variant", query.variant.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                values.push((name.to_string(), value.to_string()));
            }
        }
        self.read("catalog.search", "/store/packages", &values)
    }

    pub fn import_release(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "release.import",
            "POST",
            "/store/releases:import",
            body,
            &[],
        )
    }

    pub fn validate_release(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "release.validate",
            "POST",
            "/store/releases:validate",
            body,
            &[],
        )
    }

    pub fn install_release(&self, input: StoreInstallInput) -> Result<ApiSuccess, ApiError> {
        if input.service_id.trim().is_empty() || input.target_node_id.trim().is_empty() {
            return Err(ApiError::InvalidRequest(
                "service_id and target_node_id are required".to_string(),
            ));
        }
        self.mutate(
            "release.install",
            "POST",
            "/store/releases:install",
            serde_json::to_value(input).map_err(|err| ApiError::InvalidRequest(err.to_string()))?,
            &[],
        )
    }

    pub fn upgrade_release(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "release.upgrade",
            "POST",
            "/store/releases:upgrade",
            body,
            &[],
        )
    }

    pub fn rollback_release(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "release.rollback",
            "POST",
            "/store/releases:rollback",
            body,
            &[],
        )
    }

    pub fn delete_release(&self, service_id: &str, version: &str) -> Result<ApiSuccess, ApiError> {
        if service_id.trim().is_empty() || version.trim().is_empty() {
            return Err(ApiError::InvalidRequest(
                "release deletion requires service_id and version".to_string(),
            ));
        }
        self.mutate(
            "release.delete",
            "POST",
            "/store/releases:delete",
            json!({"service_id": service_id, "version": version}),
            &[],
        )
    }

    pub fn list_operations(&self, cursor: Option<&str>) -> Result<ApiSuccess, ApiError> {
        self.read("operation.logs", "/operations", &cursor_query(cursor))
    }

    pub fn operation(&self, operation_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "operation.logs",
            &format!("/operations/{}", path_segment(operation_id)?),
            &[],
        )
    }

    pub fn operation_logs(&self, operation_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "operation.logs",
            &format!("/operations/{}/logs", path_segment(operation_id)?),
            &[],
        )
    }

    pub fn operation_events(
        &self,
        operation_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<ApiSuccess, ApiError> {
        self.require_capability("operation.events")?;
        let mut headers = vec![("Accept".to_string(), "text/event-stream".to_string())];
        if let Some(last_event_id) = last_event_id.filter(|value| !value.trim().is_empty()) {
            headers.push(("Last-Event-ID".to_string(), last_event_id.to_string()));
        }
        let response = self.send(
            "GET",
            &format!("/operations/{}/events", path_segment(operation_id)?),
            &[],
            Value::Null,
            &headers,
        )?;
        if !(200..300).contains(&response.status) {
            return self.decode(response);
        }
        decode_sse(response)
    }

    pub fn plan_operation(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate("operation.plan", "POST", "/operations:plan", body, &[])
    }

    pub fn mutate_operation(
        &self,
        operation_id: &str,
        action: &str,
    ) -> Result<ApiSuccess, ApiError> {
        if !matches!(
            action,
            "confirm" | "apply" | "cancel" | "retry" | "rollback"
        ) {
            return Err(ApiError::InvalidRequest(format!(
                "invalid operation action {action}"
            )));
        }
        self.mutate(
            &format!("operation.{action}"),
            "POST",
            &format!("/operations/{}:{action}", path_segment(operation_id)?),
            json!({}),
            &[],
        )
    }

    pub fn list_nodes(&self, cursor: Option<&str>) -> Result<ApiSuccess, ApiError> {
        self.read("node.list", "/nodes", &cursor_query(cursor))
    }

    pub fn node(&self, node_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "node.health",
            &format!("/nodes/{}", path_segment(node_id)?),
            &[],
        )
    }

    pub fn node_health(&self, node_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "node.health",
            &format!("/nodes/{}/health", path_segment(node_id)?),
            &[],
        )
    }

    pub fn create_node_enrollment_code(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "node.register",
            "POST",
            "/nodes/enrollment-codes",
            body,
            &[],
        )
    }

    pub fn revoke_node_certificates(
        &self,
        node_id: &str,
        reason: &str,
    ) -> Result<ApiSuccess, ApiError> {
        if reason.trim().is_empty() {
            return Err(ApiError::InvalidRequest(
                "certificate revocation reason is required".to_string(),
            ));
        }
        self.mutate(
            "node.revoke",
            "POST",
            &format!("/nodes/{}:revoke-certificates", path_segment(node_id)?),
            json!({"reason": reason}),
            &[],
        )
    }

    pub fn drain_node(&self, node_id: &str) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "node.drain",
            "POST",
            &format!("/nodes/{}:drain", path_segment(node_id)?),
            json!({}),
            &[],
        )
    }

    pub fn remove_node(&self, node_id: &str) -> Result<ApiSuccess, ApiError> {
        self.mutate(
            "node.remove",
            "DELETE",
            &format!("/nodes/{}", path_segment(node_id)?),
            json!({}),
            &[],
        )
    }

    pub fn list_deployments(&self, cursor: Option<&str>) -> Result<ApiSuccess, ApiError> {
        self.read("deployment.list", "/deployments", &cursor_query(cursor))
    }

    pub fn deployment(&self, deployment_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "deployment.get",
            &format!("/deployments/{}", path_segment(deployment_id)?),
            &[],
        )
    }

    pub fn deployment_health(&self, deployment_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "deployment.health",
            &format!("/deployments/{}/health", path_segment(deployment_id)?),
            &[],
        )
    }

    pub fn deployment_bindings(&self, deployment_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "deployment.get",
            &format!("/deployments/{}/bindings", path_segment(deployment_id)?),
            &[],
        )
    }

    pub fn mutate_deployment(
        &self,
        deployment_id: &str,
        action: &str,
    ) -> Result<ApiSuccess, ApiError> {
        if !matches!(action, "start" | "stop" | "restart" | "uninstall") {
            return Err(ApiError::InvalidRequest(format!(
                "invalid deployment action {action}"
            )));
        }
        self.mutate(
            &format!("deployment.{action}"),
            "POST",
            &format!("/deployments/{}:{action}", path_segment(deployment_id)?),
            json!({}),
            &[],
        )
    }

    pub fn purge_resource_claim(
        &self,
        claim_id: &str,
        input: ResourcePurgeInput,
    ) -> Result<ApiSuccess, ApiError> {
        validate_resource_identifier(claim_id, "claim_id")?;
        input.validate(claim_id)?;
        self.mutate(
            "resource.purge",
            "POST",
            &format!("/resources/{}:purge", path_segment(claim_id)?),
            serde_json::to_value(input).map_err(|err| ApiError::InvalidRequest(err.to_string()))?,
            &[],
        )
    }

    pub fn list_topologies(&self, cursor: Option<&str>) -> Result<ApiSuccess, ApiError> {
        self.read("topology.export", "/topologies", &cursor_query(cursor))
    }

    pub fn topology(&self, topology_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "topology.export",
            &format!("/topologies/{}", path_segment(topology_id)?),
            &[],
        )
    }

    pub fn create_topology_draft(&self, spec: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate("topology.draft", "POST", "/topologies", spec, &[])
    }

    pub fn topology_revisions(&self, topology_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "topology.export",
            &format!("/topologies/{}/revisions", path_segment(topology_id)?),
            &[],
        )
    }

    pub fn topology_revision(
        &self,
        topology_id: &str,
        revision_id: &str,
    ) -> Result<ApiSuccess, ApiError> {
        self.read(
            "topology.export",
            &format!(
                "/topologies/{}/revisions/{}",
                path_segment(topology_id)?,
                path_segment(revision_id)?
            ),
            &[],
        )
    }

    pub fn create_topology_revision(
        &self,
        topology_id: &str,
        spec: Value,
        if_match: &str,
    ) -> Result<ApiSuccess, ApiError> {
        let if_match = required_etag(if_match, "topology revision")?;
        self.mutate(
            "topology.revision",
            "POST",
            &format!("/topologies/{}/revisions", path_segment(topology_id)?),
            spec,
            &[("If-Match".to_string(), if_match)],
        )
    }

    pub fn put_topology_draft_endpoint(
        &self,
        topology_id: &str,
        endpoint_id: &str,
        endpoint: Value,
        if_match: &str,
    ) -> Result<ApiSuccess, ApiError> {
        let if_match = required_etag(if_match, "topology endpoint edit")?;
        self.mutate(
            "topology.endpoint.edit",
            "PUT",
            &format!(
                "/topologies/{}/draft/endpoints/{}",
                path_segment(topology_id)?,
                path_segment(endpoint_id)?
            ),
            endpoint,
            &[("If-Match".to_string(), if_match)],
        )
    }

    pub fn delete_topology_draft_endpoint(
        &self,
        topology_id: &str,
        endpoint_id: &str,
        if_match: &str,
    ) -> Result<ApiSuccess, ApiError> {
        let if_match = required_etag(if_match, "topology endpoint delete")?;
        self.mutate(
            "topology.endpoint.edit",
            "DELETE",
            &format!(
                "/topologies/{}/draft/endpoints/{}",
                path_segment(topology_id)?,
                path_segment(endpoint_id)?
            ),
            json!({}),
            &[("If-Match".to_string(), if_match)],
        )
    }

    pub fn put_topology_draft_link(
        &self,
        topology_id: &str,
        source_endpoint: &str,
        target_endpoint: &str,
        link: Value,
        if_match: &str,
    ) -> Result<ApiSuccess, ApiError> {
        let if_match = required_etag(if_match, "topology link edit")?;
        self.mutate(
            "topology.link.edit",
            "PUT",
            &format!(
                "/topologies/{}/draft/links/{}/{}",
                path_segment(topology_id)?,
                path_segment(source_endpoint)?,
                path_segment(target_endpoint)?
            ),
            link,
            &[("If-Match".to_string(), if_match)],
        )
    }

    pub fn delete_topology_draft_link(
        &self,
        topology_id: &str,
        source_endpoint: &str,
        target_endpoint: &str,
        if_match: &str,
    ) -> Result<ApiSuccess, ApiError> {
        let if_match = required_etag(if_match, "topology link delete")?;
        self.mutate(
            "topology.link.edit",
            "DELETE",
            &format!(
                "/topologies/{}/draft/links/{}/{}",
                path_segment(topology_id)?,
                path_segment(source_endpoint)?,
                path_segment(target_endpoint)?
            ),
            json!({}),
            &[("If-Match".to_string(), if_match)],
        )
    }

    pub fn topology_status(&self, topology_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "topology.status",
            &format!("/topologies/{}/status", path_segment(topology_id)?),
            &[],
        )
    }

    pub fn topology_action(
        &self,
        topology_id: &str,
        action: &str,
        body: Value,
        if_match: Option<&str>,
    ) -> Result<ApiSuccess, ApiError> {
        if !matches!(action, "validate" | "diff" | "apply" | "rollback") {
            return Err(ApiError::InvalidRequest(format!(
                "invalid topology action {action}"
            )));
        }
        let headers = if matches!(action, "apply" | "rollback") {
            vec![(
                "If-Match".to_string(),
                required_etag(if_match.unwrap_or_default(), &format!("topology {action}"))?,
            )]
        } else {
            Vec::new()
        };
        self.mutate(
            &format!("topology.{action}"),
            "POST",
            &format!("/topologies/{}:{action}", path_segment(topology_id)?),
            body,
            &headers,
        )
    }

    pub fn create_diagnostic(&self, body: Value) -> Result<ApiSuccess, ApiError> {
        self.mutate("diagnostic.create", "POST", "/diagnostics", body, &[])
    }

    pub fn list_diagnostics(&self, cursor: Option<&str>) -> Result<ApiSuccess, ApiError> {
        self.read("diagnostic.list", "/diagnostics", &cursor_query(cursor))
    }

    pub fn diagnostic(&self, report_id: &str) -> Result<ApiSuccess, ApiError> {
        self.read(
            "diagnostic.get",
            &format!("/diagnostics/{}", path_segment(report_id)?),
            &[],
        )
    }

    pub fn export_diagnostic(&self, report_id: &str, format: &str) -> Result<ApiSuccess, ApiError> {
        let extension = match format {
            "json" => "json",
            "markdown" | "md" => "md",
            _ => {
                return Err(ApiError::InvalidRequest(
                    "diagnostic export format must be json or markdown".to_string(),
                ));
            }
        };
        self.read(
            "diagnostic.export",
            &format!("/diagnostics/{}.{}", path_segment(report_id)?, extension),
            &[],
        )
    }

    fn read(
        &self,
        capability: &str,
        path: &str,
        query: &[(String, String)],
    ) -> Result<ApiSuccess, ApiError> {
        self.require_capability(capability)?;
        self.get(path, query)
    }

    fn get(&self, path: &str, query: &[(String, String)]) -> Result<ApiSuccess, ApiError> {
        self.request("GET", path, query, Value::Null, &[])
    }

    fn mutate(
        &self,
        capability: &str,
        method: &str,
        path: &str,
        body: Value,
        headers: &[(String, String)],
    ) -> Result<ApiSuccess, ApiError> {
        self.require_capability(capability)?;
        let mut headers = headers.to_vec();
        headers.push(("Idempotency-Key".to_string(), next_idempotency_key()));
        self.request(method, path, &[], body, &headers)
    }

    fn require_capability(&self, capability: &str) -> Result<(), ApiError> {
        if self.capabilities(false)?.supports(capability) {
            Ok(())
        } else {
            Err(ApiError::CapabilityUnavailable {
                action: capability.to_string(),
            })
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
        extra_headers: &[(String, String)],
    ) -> Result<ApiSuccess, ApiError> {
        let response = self.send(method, path, query, body, extra_headers)?;
        self.decode(response)
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: Value,
        extra_headers: &[(String, String)],
    ) -> Result<TransportResponse, ApiError> {
        if !path.starts_with('/') {
            return Err(ApiError::InvalidRequest(
                "API path must start with /".to_string(),
            ));
        }
        let mut url = format!("{}{}", self.config.base_url, path);
        if !query.is_empty() {
            let query = query
                .iter()
                .map(|(name, value)| {
                    format!("{}={}", query_component(name), query_component(value))
                })
                .collect::<Vec<_>>()
                .join("&");
            url.push('?');
            url.push_str(&query);
        }
        let mut headers = BTreeMap::from([
            (
                "Accept".to_string(),
                "application/json, application/problem+json".to_string(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "User-Agent".to_string(),
                "ojos-orchestrator-tui/1.0".to_string(),
            ),
        ]);
        if let Some(token) = self.config.bearer_token.as_deref() {
            if token.is_empty()
                || token.len() > 16 * 1024
                || token
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace())
            {
                return Err(ApiError::InvalidConfiguration(
                    "bearer token is not safe for an Authorization header".to_string(),
                ));
            }
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        headers.extend(extra_headers.iter().cloned());
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).map_err(|err| ApiError::InvalidRequest(err.to_string()))?
        };
        self.transport.execute(TransportRequest {
            method: method.to_string(),
            url,
            headers,
            body,
        })
    }

    fn decode(&self, response: TransportResponse) -> Result<ApiSuccess, ApiError> {
        if !(200..300).contains(&response.status) {
            if response.headers.get("content-type").is_some_and(|value| {
                !value
                    .to_ascii_lowercase()
                    .starts_with("application/problem+json")
            }) {
                return Err(ApiError::InvalidResponse(
                    "v1 error response must use application/problem+json".to_string(),
                ));
            }
            let problem =
                serde_json::from_slice::<ProblemDetails>(&response.body).unwrap_or_else(|_| {
                    ProblemDetails {
                        r#type: String::new(),
                        title: String::new(),
                        status: response.status,
                        code: "HTTP_ERROR".to_string(),
                        detail: String::from_utf8_lossy(&response.body).trim().to_string(),
                        request_id: response
                            .headers
                            .get("x-request-id")
                            .cloned()
                            .unwrap_or_default(),
                        operation_id: None,
                    }
                });
            return Err(ApiError::Problem {
                status: response.status,
                code: if problem.code.is_empty() {
                    "HTTP_ERROR".to_string()
                } else {
                    problem.code
                },
                detail: problem.detail,
                request_id: problem.request_id,
                operation_id: problem.operation_id,
                retry_after: response.headers.get("retry-after").cloned(),
            });
        }
        if response
            .headers
            .get("content-type")
            .is_some_and(|value| !value.to_ascii_lowercase().starts_with("application/json"))
        {
            return Err(ApiError::InvalidResponse(
                "v1 success response must use application/json".to_string(),
            ));
        }
        let envelope = serde_json::from_slice::<Envelope>(&response.body)
            .map_err(|err| ApiError::InvalidResponse(format!("invalid v1 envelope: {err}")))?;
        if envelope.meta.api_version != "v1" || envelope.meta.request_id.trim().is_empty() {
            return Err(ApiError::InvalidResponse(
                "response meta must contain api_version=v1 and request_id".to_string(),
            ));
        }
        let mut meta = envelope.meta;
        if meta.next_cursor.is_none() {
            meta.next_cursor = envelope
                .data
                .get("next_cursor")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if response.status == 202
            && envelope
                .data
                .get("operation_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(ApiError::InvalidResponse(
                "HTTP 202 response must contain data.operation_id".to_string(),
            ));
        }
        Ok(ApiSuccess {
            status: response.status,
            data: envelope.data,
            meta,
            etag: response.headers.get("etag").cloned(),
        })
    }
}

fn normalize_base_url(value: &str) -> Result<String, ApiError> {
    let value = value.trim().trim_end_matches('/');
    let uri = value.parse::<Uri>().map_err(|error| {
        ApiError::InvalidConfiguration(format!("api_url is not a valid URL: {error}"))
    })?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let host = uri.host().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if uri.authority().is_none()
        || host.is_empty()
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
        || (scheme != "https" && !(scheme == "http" && loopback))
    {
        return Err(ApiError::InvalidConfiguration(
            "api_url must be HTTPS (HTTP is allowed only for loopback), contain a host, and contain no embedded credentials".to_string(),
        ));
    }
    if uri.query().is_some() || !matches!(uri.path(), "" | "/" | "/api/v1" | "/api/v1/") {
        return Err(ApiError::InvalidConfiguration(
            "api_url path must be empty or /api/v1 and must not contain a query".to_string(),
        ));
    }
    let base = if value.ends_with("/api/v1") {
        value.to_string()
    } else {
        format!("{value}/api/v1")
    };
    Ok(base)
}

fn cursor_query(cursor: Option<&str>) -> Vec<(String, String)> {
    let mut query = vec![("limit".to_string(), "100".to_string())];
    if let Some(cursor) = cursor.filter(|value| !value.trim().is_empty()) {
        query.push(("cursor".to_string(), cursor.to_string()));
    }
    query
}

fn path_segment(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':'))
        })
    {
        return Err(ApiError::InvalidRequest(format!(
            "invalid resource identifier {value:?}"
        )));
    }
    Ok(value.to_string())
}

fn validate_resource_identifier(value: &str, field: &str) -> Result<(), ApiError> {
    let length = value.len();
    if value.trim() != value
        || !(2..=180).contains(&length)
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || value.bytes().any(|byte| {
            !(byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b':' | b'-'))
        })
    {
        return Err(ApiError::InvalidRequest(format!(
            "resource purge {field} does not match the ResourceClaim identifier contract"
        )));
    }
    Ok(())
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn required_etag(value: &str, subject: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.starts_with("W/") {
        return Err(ApiError::InvalidRequest(format!(
            "{subject} requires a strong ETag/If-Match value"
        )));
    }
    if value.starts_with('"') && value.ends_with('"') && value.len() > 2 {
        return Ok(value.to_string());
    }
    Ok(format!("\"{}\"", path_segment(value)?))
}

fn decode_sse(response: TransportResponse) -> Result<ApiSuccess, ApiError> {
    if !response
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"))
    {
        return Err(ApiError::InvalidResponse(
            "Operation events response is not text/event-stream".to_string(),
        ));
    }
    let request_id = response
        .headers
        .get("x-request-id")
        .cloned()
        .ok_or_else(|| {
            ApiError::InvalidResponse("SSE response is missing X-Request-ID".to_string())
        })?;
    let source = std::str::from_utf8(&response.body)
        .map_err(|err| ApiError::InvalidResponse(format!("SSE is not UTF-8: {err}")))?
        .replace("\r\n", "\n");
    let mut events = Vec::new();
    let mut last_event_id = None;
    for block in source.split("\n\n") {
        let mut id = String::new();
        let mut event = "message".to_string();
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if line.starts_with(':') || line.starts_with("retry:") {
                continue;
            }
            if let Some(value) = line.strip_prefix("id:") {
                id = value.trim_start().to_string();
            } else if let Some(value) = line.strip_prefix("event:") {
                event = value.trim_start().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                data_lines.push(value.trim_start());
            }
        }
        if data_lines.is_empty() {
            continue;
        }
        let data = serde_json::from_str::<Value>(&data_lines.join("\n"))
            .map_err(|err| ApiError::InvalidResponse(format!("invalid SSE data JSON: {err}")))?;
        if !id.is_empty() {
            last_event_id = Some(id.clone());
        }
        events.push(SseEvent { id, event, data });
    }
    Ok(ApiSuccess {
        status: response.status,
        data: json!({"items": events}),
        meta: ResponseMeta {
            request_id,
            api_version: "v1".to_string(),
            next_cursor: last_event_id,
        },
        etag: None,
    })
}

fn query_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![byte as char]
            } else {
                format!("%{byte:02X}").chars().collect::<Vec<_>>()
            }
        })
        .collect()
}

fn next_idempotency_key() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = IDEMPOTENCY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("tui-{timestamp:032x}-{sequence:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FixtureTransport {
        responses: Mutex<VecDeque<TransportResponse>>,
        requests: Mutex<Vec<TransportRequest>>,
    }

    impl FixtureTransport {
        fn from_bodies(bodies: impl IntoIterator<Item = (u16, &'static str)>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(
                    bodies
                        .into_iter()
                        .map(|(status, body)| TransportResponse {
                            status,
                            headers: BTreeMap::new(),
                            body: body.as_bytes().to_vec(),
                        })
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl ApiTransport for FixtureTransport {
        fn execute(&self, request: TransportRequest) -> Result<TransportResponse, ApiError> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ApiError::Transport("fixture response exhausted".to_string()))
        }
    }

    fn config() -> ApiClientConfig {
        ApiClientConfig::new("https://control.example/api/v1").unwrap()
    }

    #[test]
    fn capability_fixture_filters_unsupported_entries() {
        let transport = FixtureTransport::from_bodies([(
            200,
            include_str!("../tests/fixtures/capabilities.json"),
        )]);
        let client = ApiClient::with_transport(config(), transport);
        let capabilities = client.capabilities(false).unwrap();
        assert!(capabilities.supports("operation.retry"));
        assert!(!capabilities.supports("legacy.fake"));
    }

    #[test]
    fn capability_fixture_covers_the_complete_published_v1_contract() {
        let transport = FixtureTransport::from_bodies([(
            200,
            include_str!("../tests/fixtures/capabilities.json"),
        )]);
        let client = ApiClient::with_transport(config(), transport);
        let fixture_actions = client.capabilities(false).unwrap().actions();
        let published_actions = orchestrator_legacy::V1_ACTIONS
            .iter()
            .map(|descriptor| descriptor.action_id.to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(fixture_actions, published_actions);
    }

    #[test]
    fn process_memory_access_token_is_sent_as_bearer() {
        let transport = FixtureTransport::from_bodies([(
            200,
            include_str!("../tests/fixtures/capabilities.json"),
        )]);
        let mut config = config();
        config.bearer_token = Some("opaque-access-token".to_string());
        let client = ApiClient::with_transport(config, transport.clone());
        client.capabilities(false).unwrap();
        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[0].headers.get("Authorization").map(String::as_str),
            Some("Bearer opaque-access-token")
        );
    }

    #[test]
    fn bearer_header_injection_is_rejected_before_transport() {
        let transport = FixtureTransport::from_bodies([(
            200,
            include_str!("../tests/fixtures/capabilities.json"),
        )]);
        let mut config = config();
        config.bearer_token = Some("bad\r\nheader".to_string());
        let client = ApiClient::with_transport(config, transport.clone());
        assert!(matches!(
            client.capabilities(false),
            Err(ApiError::InvalidConfiguration(_))
        ));
        assert!(transport.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn operation_retry_uses_v1_action_path_and_idempotency_key() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                202,
                include_str!("../tests/fixtures/operation-accepted.json"),
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let result = client.mutate_operation("op-42", "retry").unwrap();
        assert_eq!(result.status, 202);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[1].url,
            "https://control.example/api/v1/operations/op-42:retry"
        );
        assert!(requests[1].headers.contains_key("Idempotency-Key"));
    }

    #[test]
    fn resource_purge_requires_exact_confirmation_and_sends_no_secret_fields() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                202,
                include_str!("../tests/fixtures/operation-accepted.json"),
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let claim_id = "claim-1";
        let digest = format!("sha256:{}", "a".repeat(64));
        let confirmation = format!("PURGE {claim_id} {digest} GENERATION 7");
        let input = ResourcePurgeInput {
            node_id: "node-1".to_string(),
            claim_digest: digest.clone(),
            generation: 7,
            confirmation: format!("{confirmation} "),
            reason: "approved cleanup after retention review".to_string(),
        };

        assert!(matches!(
            client.purge_resource_claim(claim_id, input.clone()),
            Err(ApiError::InvalidRequest(message)) if message.contains("exactly equal")
        ));
        // Exact confirmation is validated before even the capability lookup.
        assert!(transport.requests.lock().unwrap().is_empty());

        client
            .purge_resource_claim(
                claim_id,
                ResourcePurgeInput {
                    confirmation,
                    ..input
                },
            )
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        let request = &requests[1];
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.url,
            "https://control.example/api/v1/resources/claim-1:purge"
        );
        assert!(request.headers.contains_key("Idempotency-Key"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body.as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "claim_digest".to_string(),
                "confirmation".to_string(),
                "generation".to_string(),
                "node_id".to_string(),
                "reason".to_string(),
            ])
        );
        for forbidden in [
            "actor",
            "actor_id",
            "dsn",
            "password",
            "secret",
            "secret_refs",
        ] {
            assert!(
                body.get(forbidden).is_none(),
                "purge body leaked {forbidden}"
            );
        }
    }

    #[test]
    fn missing_capability_never_calls_mutation_endpoint() {
        let transport = FixtureTransport::from_bodies([(
            200,
            include_str!("../tests/fixtures/capabilities-minimal.json"),
        )]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let error = client.drain_node("node-1").unwrap_err();
        assert_eq!(
            error,
            ApiError::CapabilityUnavailable {
                action: "node.drain".to_string()
            }
        );
        assert_eq!(transport.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn topology_apply_requires_and_forwards_etag() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                202,
                include_str!("../tests/fixtures/operation-accepted.json"),
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        assert!(matches!(
            client.topology_action("main", "apply", json!({}), None),
            Err(ApiError::InvalidRequest(_))
        ));
        client
            .topology_action("main", "apply", json!({}), Some("\"rev-7\""))
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[1].headers.get("If-Match").map(String::as_str),
            Some("\"rev-7\"")
        );
    }

    #[test]
    fn problem_fixture_preserves_code_request_and_retry_after() {
        let transport =
            FixtureTransport::from_bodies([(503, include_str!("../tests/fixtures/problem.json"))]);
        transport
            .responses
            .lock()
            .unwrap()
            .front_mut()
            .unwrap()
            .headers
            .insert("retry-after".to_string(), "5".to_string());
        let client = ApiClient::with_transport(config(), transport);
        let error = client.list_nodes(None).unwrap_err();
        assert!(matches!(
            error,
            ApiError::Problem {
                status: 503,
                ref code,
                ref request_id,
                ..
            } if code == "CONTROL_PLANE_RECOVERING" && request_id == "req-fixture-problem"
        ));
        assert_eq!(error.retry_after(), Some("5"));
    }

    #[test]
    fn managed_install_defaults_match_v1_contract() {
        let input = StoreInstallInput::managed("gateway", "node-a");
        let value = serde_json::to_value(input).unwrap();
        assert_eq!(value["mode"], "MANAGED");
        assert_eq!(value["start"], true);
        assert_eq!(value["migration_policy"], "APPLY");
        assert_eq!(value["config"], json!({}));
        assert_eq!(value["secret_refs"], json!({}));
        assert!(value.get("endpoint").is_none());
        assert!(value.get("bindings").is_none());
        assert!(value.get("topology").is_none());

        let legacy_catalog_command = serde_json::to_value(CatalogSourceInput::trusted(
            "stable",
            "https://catalog.example/v2.json",
            "key-1",
        ))
        .unwrap();
        assert!(legacy_catalog_command.get("public_key").is_none());
    }

    #[test]
    fn managed_install_sends_explicit_bindings_and_topology_revision() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                202,
                include_str!("../tests/fixtures/operation-accepted.json"),
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let mut input = StoreInstallInput::managed("judge-worker", "node-b");
        input.bindings = vec![InstallApiBindingSelection {
            name: "judge_control".to_string(),
            provider_deployment_id: "judge-a".to_string(),
        }];
        input.topology_id = Some("primary".to_string());
        input.topology_etag = Some("\"revision-7\"".to_string());
        input.apply_pipeline_options(&StorePipelineOptions {
            start: false,
            migration_policy: "DRY_RUN".to_string(),
            gateway_node_id: Some("gateway-a".to_string()),
            config: json!({"namespace": "contest"}),
            secret_refs: BTreeMap::from([(
                "signing_key".to_string(),
                "secrets/judge/signing-key".to_string(),
            )]),
        });

        client.install_release(input).unwrap();

        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[1].url,
            "https://control.example/api/v1/store/releases:install"
        );
        assert!(requests[1].headers.contains_key("Idempotency-Key"));
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(
            body["bindings"],
            json!([{"name":"judge_control","provider_deployment_id":"judge-a"}])
        );
        assert_eq!(body["topology_id"], json!("primary"));
        assert_eq!(body["topology_etag"], json!("\"revision-7\""));
        assert_eq!(body["start"], json!(false));
        assert_eq!(body["migration_policy"], json!("DRY_RUN"));
        assert_eq!(body["gateway_node_id"], json!("gateway-a"));
        assert_eq!(body["config"], json!({"namespace": "contest"}));
        assert_eq!(
            body["secret_refs"],
            json!({"signing_key": "secrets/judge/signing-key"})
        );
    }

    #[test]
    fn catalog_bootstrap_key_must_be_canonical_padded_base64_for_32_bytes() {
        let client = ApiClient::with_transport(
            config(),
            FixtureTransport::from_bodies(std::iter::empty::<(u16, &'static str)>()),
        );
        let mut source = CatalogSourceInput::trusted(
            "bootstrap",
            "https://catalog.example/v2.json",
            "bootstrap-key",
        );
        source.public_key = Some("not-base64".to_string());
        assert!(matches!(
            client.register_catalog(source),
            Err(ApiError::InvalidRequest(message)) if message.contains("canonical padded base64")
        ));
    }

    #[test]
    fn catalog_and_store_mutations_use_exact_v1_routes_and_payloads() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                201,
                r#"{"data":{"source":{"id":"stable"}},"meta":{"request_id":"req-catalog","api_version":"v1"}}"#,
            ),
            (
                202,
                r#"{"data":{"operation_id":"op-upgrade"},"meta":{"request_id":"req-upgrade","api_version":"v1"}}"#,
            ),
            (
                202,
                r#"{"data":{"operation_id":"op-rollback"},"meta":{"request_id":"req-rollback","api_version":"v1"}}"#,
            ),
            (
                200,
                r#"{"data":{"deleted":true},"meta":{"request_id":"req-delete","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let mut catalog_source =
            CatalogSourceInput::trusted("stable", "https://catalog.example/v2.json", "key-1");
        catalog_source.public_key =
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string());
        client.register_catalog(catalog_source).unwrap();
        client
            .upgrade_release(json!({
                "deployment_id": "dep-1",
                "version": "2.0.0",
                "catalog_source_id": "stable",
            }))
            .unwrap();
        client
            .rollback_release(json!({
                "deployment_id": "dep-1",
                "version": "1.0.0",
                "catalog_source_id": "stable",
            }))
            .unwrap();
        client.delete_release("api", "1.0.0").unwrap();

        let requests = transport.requests.lock().unwrap();
        let catalog: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(
            requests[1].url,
            "https://control.example/api/v1/store/catalogs"
        );
        assert_eq!(catalog["required_key_id"], "key-1");
        assert_eq!(
            catalog["public_key"],
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        );
        assert!(requests[1].headers.contains_key("Idempotency-Key"));
        let upgrade: Value = serde_json::from_slice(&requests[2].body).unwrap();
        assert_eq!(
            requests[2].url,
            "https://control.example/api/v1/store/releases:upgrade"
        );
        assert_eq!(upgrade["deployment_id"], "dep-1");
        assert!(upgrade.get("target_node_id").is_none());
        let deletion: Value = serde_json::from_slice(&requests[4].body).unwrap();
        assert_eq!(deletion, json!({"service_id": "api", "version": "1.0.0"}));
    }

    #[test]
    fn release_import_uses_only_a_trusted_catalog_selection() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                201,
                r#"{"data":{"imported":[]},"meta":{"request_id":"req-import","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let input: Value =
            serde_json::from_str(include_str!("../tests/fixtures/store-import-request.json"))
                .unwrap();
        client.import_release(input.clone()).unwrap();
        let requests = transport.requests.lock().unwrap();
        let sent: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(sent, input);
        assert!(sent.get("source_url").is_none());
        assert!(sent.get("checksum").is_none());
        assert!(sent.get("metadata_sha256").is_none());
    }

    #[test]
    fn deployment_health_is_a_capability_gated_get_and_cursor_is_preserved() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                200,
                r#"{"data":{"items":[],"next_cursor":"dep-9"},"meta":{"request_id":"req-list","api_version":"v1"}}"#,
            ),
            (
                200,
                r#"{"data":{"deployment_id":"dep-1","health":"healthy"},"meta":{"request_id":"req-health","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let page = client.list_deployments(Some("dep-3")).unwrap();
        assert_eq!(page.meta.next_cursor.as_deref(), Some("dep-9"));
        client.deployment_health("dep-1").unwrap();

        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[1].method, "GET");
        assert!(
            requests[1]
                .url
                .ends_with("/deployments?limit=100&cursor=dep-3")
        );
        assert_eq!(requests[2].method, "GET");
        assert!(requests[2].url.ends_with("/deployments/dep-1/health"));
        assert!(!requests[2].headers.contains_key("Idempotency-Key"));
    }

    #[test]
    fn deployment_bindings_uses_deployment_get_capability_without_mutation_headers() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                200,
                r#"{"data":{"deployment_id":"worker-b","service_id":"judge-worker","items":[{"binding_id":"binding-1","requirement_name":"judge_control","context_generation":3,"credential_generation":4,"health":"HEALTHY","drift":[]}]},"meta":{"request_id":"req-bindings","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());

        let response = client.deployment_bindings("worker-b").unwrap();

        assert_eq!(response.data["items"][0]["context_generation"], 3);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[1].url,
            "https://control.example/api/v1/deployments/worker-b/bindings"
        );
        assert_eq!(requests[1].method, "GET");
        assert!(!requests[1].headers.contains_key("Idempotency-Key"));
    }

    #[test]
    fn topology_revision_and_actions_forward_strong_etags_and_exact_bodies() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                201,
                r#"{"data":{"revision":{"revision_id":"rev-8"}},"meta":{"request_id":"req-revision","api_version":"v1"}}"#,
            ),
            (
                200,
                r#"{"data":{"valid":true},"meta":{"request_id":"req-validate","api_version":"v1"}}"#,
            ),
            (
                202,
                r#"{"data":{"operation_id":"op-rollback"},"meta":{"request_id":"req-rollback","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        let spec = json!({"api_version":"v1","topology_id":"main"});
        client
            .create_topology_revision("main", spec.clone(), "rev-7")
            .unwrap();
        client
            .topology_action("main", "validate", spec, None)
            .unwrap();
        client
            .topology_action(
                "main",
                "rollback",
                json!({"revision_id":"rev-2"}),
                Some("\"rev-8\""),
            )
            .unwrap();

        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[1].headers.get("If-Match").map(String::as_str),
            Some("\"rev-7\"")
        );
        assert_eq!(
            requests[1].url,
            "https://control.example/api/v1/topologies/main/revisions"
        );
        assert!(requests[2].url.ends_with("/topologies/main:validate"));
        let rollback: Value = serde_json::from_slice(&requests[3].body).unwrap();
        assert_eq!(rollback, json!({"revision_id":"rev-2"}));
    }

    #[test]
    fn operation_sse_preserves_last_event_id_and_returns_reconnect_cursor() {
        let transport = Arc::new(FixtureTransport {
            responses: Mutex::new(VecDeque::from([
                TransportResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: include_bytes!("../tests/fixtures/capabilities.json").to_vec(),
                },
                TransportResponse {
                    status: 200,
                    headers: BTreeMap::from([
                        ("content-type".to_string(), "text/event-stream".to_string()),
                        ("x-request-id".to_string(), "req-events".to_string()),
                    ]),
                    body: b"id: cursor-2\nevent: job\ndata: {\"event\":{\"sequence\":2}}\n\nretry: 1000\n\n".to_vec(),
                },
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let client = ApiClient::with_transport(config(), transport.clone());
        let response = client.operation_events("op-1", Some("cursor-1")).unwrap();
        assert_eq!(response.meta.next_cursor.as_deref(), Some("cursor-2"));
        assert_eq!(response.data["items"][0]["event"], "job");
        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[1].headers.get("Last-Event-ID").map(String::as_str),
            Some("cursor-1")
        );
        assert_eq!(
            requests[1].headers.get("Accept").map(String::as_str),
            Some("text/event-stream")
        );
    }

    #[test]
    fn accepted_response_without_operation_id_is_rejected() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                202,
                r#"{"data":{"status":"QUEUED"},"meta":{"request_id":"req-invalid","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport);
        assert!(matches!(
            client.mutate_operation("op-1", "retry"),
            Err(ApiError::InvalidResponse(message)) if message.contains("operation_id")
        ));
    }

    #[test]
    fn node_identity_and_diagnostic_controls_use_published_v1_actions() {
        let transport = FixtureTransport::from_bodies([
            (200, include_str!("../tests/fixtures/capabilities.json")),
            (
                201,
                r#"{"data":{"enrollment_code":"secret"},"meta":{"request_id":"req-enroll","api_version":"v1"}}"#,
            ),
            (
                200,
                r#"{"data":{"certificate_status":"REVOKED"},"meta":{"request_id":"req-revoke","api_version":"v1"}}"#,
            ),
            (
                201,
                r#"{"data":{"action_result":{"status":"SUCCEEDED"}},"meta":{"request_id":"req-create-diagnostic","api_version":"v1"}}"#,
            ),
            (
                200,
                r#"{"data":{"diagnostic_report":{"report_id":"diag-1"}},"meta":{"request_id":"req-diagnostic","api_version":"v1"}}"#,
            ),
        ]);
        let client = ApiClient::with_transport(config(), transport.clone());
        client
            .create_node_enrollment_code(json!({
                "node_id": "edge-1",
                "host_ip": "10.0.0.8",
                "role": "standalone",
                "ttl_seconds": 600,
            }))
            .unwrap();
        client
            .revoke_node_certificates("edge-1", "operator requested rotation")
            .unwrap();
        client.create_diagnostic(json!({})).unwrap();
        client.diagnostic("diag-1").unwrap();

        let requests = transport.requests.lock().unwrap();
        assert!(requests[1].url.ends_with("/nodes/enrollment-codes"));
        assert_eq!(requests[2].method, "POST");
        assert!(
            requests[2]
                .url
                .ends_with("/nodes/edge-1:revoke-certificates")
        );
        let revoke: Value = serde_json::from_slice(&requests[2].body).unwrap();
        assert_eq!(revoke["reason"], "operator requested rotation");
        assert!(requests[3].url.ends_with("/diagnostics"));
        let diagnostic: Value = serde_json::from_slice(&requests[3].body).unwrap();
        assert_eq!(diagnostic, json!({}));
        assert!(diagnostic.get("operation_id").is_none());
        assert!(requests[4].url.ends_with("/diagnostics/diag-1"));
    }

    #[test]
    fn base_url_rejects_embedded_credentials_and_does_not_duplicate_prefix() {
        assert!(ApiClientConfig::new("https://user@example.test").is_err());
        assert!(ApiClientConfig::new("http://control.example").is_err());
        assert!(ApiClientConfig::new("https://example.test/unexpected").is_err());
        assert_eq!(
            ApiClientConfig::new("http://127.0.0.1:8080")
                .unwrap()
                .base_url,
            "http://127.0.0.1:8080/api/v1"
        );
        assert_eq!(
            ApiClientConfig::new("https://example.test/api/v1/")
                .unwrap()
                .base_url,
            "https://example.test/api/v1"
        );
    }
}
