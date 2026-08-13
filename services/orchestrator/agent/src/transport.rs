use crate::{
    NodeRuntimeFactsPublisher, NodeRuntimeFactsV1, RuntimePolicyError,
    WorkloadCredentialExchangeRequest, WorkloadCredentialExchanger,
};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use orchestrator_control_plane::{CompleteRequest, HeartbeatRequest, JobKind, NewJobEvent};
use orchestrator_runtime::ArtifactReference;
use orchestrator_runtime::WorkloadCredential;
use reqwest::redirect::Policy;
use reqwest::{Certificate, Client, Identity, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;
use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("failed to read mTLS material: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid agent transport configuration: {0}")]
    Configuration(String),
    #[error("agent protocol request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("control plane rejected the agent request ({status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("invalid agent protocol response: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LeasedJob {
    pub job_id: String,
    pub kind: JobKind,
    pub payload: Value,
    pub payload_sha256: String,
    pub lease_token: String,
    pub lease_expires_at_ms: i64,
}

impl LeasedJob {
    #[cfg(test)]
    pub(crate) fn new_for_test(
        job_id: &str,
        kind: JobKind,
        payload: Value,
        lease_token: &str,
    ) -> Self {
        let payload_sha256 = orchestrator_control_plane::canonical_payload_sha256(&payload);
        Self {
            job_id: job_id.to_string(),
            kind,
            payload,
            payload_sha256,
            lease_token: lease_token.to_string(),
            lease_expires_at_ms: i64::MAX,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClaimResponse {
    pub jobs: Vec<LeasedJob>,
    pub retry_after_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentClaimRequest {
    pub node_id: String,
    pub instance_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatAck {
    pub cancel_requested: bool,
}

#[async_trait]
pub trait AgentTransport: Send + Sync {
    async fn claim(&self, request: AgentClaimRequest) -> Result<ClaimResponse, TransportError>;
    async fn heartbeat(
        &self,
        node_id: &str,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatAck, TransportError>;
    async fn complete(&self, node_id: &str, request: CompleteRequest)
    -> Result<(), TransportError>;
}

pub struct DownloadedArtifact {
    file: NamedTempFile,
}

impl DownloadedArtifact {
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, TransportError> {
        let mut file = NamedTempFile::new()?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(Self { file })
    }
}

#[async_trait]
pub trait ArtifactFetcher: Send + Sync {
    async fn download(
        &self,
        job: &LeasedJob,
        reference: &ArtifactReference,
    ) -> Result<DownloadedArtifact, TransportError>;
}

/// Production HTTP client for the internal mTLS-only agent protocol.
///
/// Both the client identity and the configured control-plane CA are mandatory;
/// this constructor deliberately rejects plaintext HTTP and system-root-only
/// configurations.
#[derive(Clone)]
pub struct HttpMtlsTransport {
    inner: ProtocolHttpTransport,
}

#[derive(Clone)]
pub struct LoopbackHttpTransport {
    inner: ProtocolHttpTransport,
}

#[derive(Clone)]
pub struct EnrollmentClient {
    inner: ProtocolHttpTransport,
}

#[derive(Clone)]
pub struct HttpArtifactFetcher {
    inner: ProtocolHttpTransport,
    node_id: String,
}

#[derive(Clone)]
pub struct HttpWorkloadCredentialExchanger {
    inner: ProtocolHttpTransport,
    node_id: String,
}

#[derive(Clone)]
pub struct HttpNodeRuntimeFactsPublisher {
    inner: ProtocolHttpTransport,
    node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NodeCertificateBundle {
    pub node_id: String,
    pub spiffe_id: String,
    pub serial_hex: String,
    pub certificate_pem: String,
    pub ca_certificate_pem: String,
    pub not_after_ms: i64,
    pub renew_after_ms: i64,
}

#[derive(Clone)]
struct ProtocolHttpTransport {
    base_url: Url,
    client: Client,
    local_bootstrap_secret: Option<String>,
}

impl HttpMtlsTransport {
    pub fn from_pem_files(
        control_plane: &str,
        certificate_path: impl AsRef<Path>,
        private_key_path: impl AsRef<Path>,
        ca_path: impl AsRef<Path>,
    ) -> Result<Self, TransportError> {
        let certificate = fs::read(certificate_path)?;
        let private_key = fs::read(private_key_path)?;
        let ca_bundle = fs::read(ca_path)?;
        Self::from_pem(control_plane, &certificate, &private_key, &ca_bundle)
    }

    pub fn from_pem(
        control_plane: &str,
        certificate_pem: &[u8],
        private_key_pem: &[u8],
        ca_bundle_pem: &[u8],
    ) -> Result<Self, TransportError> {
        let mut base_url = Url::parse(control_plane)
            .map_err(|error| TransportError::Configuration(error.to_string()))?;
        if base_url.scheme() != "https" {
            return Err(TransportError::Configuration(
                "--control-plane must use https for the mTLS agent protocol".to_string(),
            ));
        }
        if base_url.cannot_be_a_base() || base_url.host_str().is_none() {
            return Err(TransportError::Configuration(
                "--control-plane must be an absolute HTTPS URL".to_string(),
            ));
        }
        base_url.set_query(None);
        base_url.set_fragment(None);

        let mut identity_pem =
            Vec::with_capacity(certificate_pem.len() + private_key_pem.len() + 1);
        identity_pem.extend_from_slice(certificate_pem);
        if !identity_pem.ends_with(b"\n") {
            identity_pem.push(b'\n');
        }
        identity_pem.extend_from_slice(private_key_pem);
        let identity = Identity::from_pem(&identity_pem)?;
        let roots = Certificate::from_pem_bundle(ca_bundle_pem)?;
        if roots.is_empty() {
            return Err(TransportError::Configuration(
                "the CA file did not contain a certificate".to_string(),
            ));
        }
        let builder = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .identity(identity)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(40))
            .tls_certs_only(roots);
        let client = builder.build()?;
        Ok(Self {
            inner: ProtocolHttpTransport {
                base_url,
                client,
                local_bootstrap_secret: None,
            },
        })
    }

    pub async fn renew_certificate(
        &self,
        csr_pem: &str,
    ) -> Result<NodeCertificateBundle, TransportError> {
        if csr_pem.is_empty() || csr_pem.len() > 64 * 1024 {
            return Err(TransportError::Configuration(
                "CSR must contain 1-65536 bytes".to_string(),
            ));
        }
        let url = self.inner.endpoint(&["certificates:renew"])?;
        let response = self
            .inner
            .post(url)
            .json(&CertificateRequest { csr_pem })
            .send()
            .await?;
        accepted_status(response, reqwest::StatusCode::OK)
            .await?
            .json()
            .await
            .map_err(Into::into)
    }

    /// Commits a replacement certificate only after it has been durably
    /// stored and can authenticate. The call is idempotent for the active
    /// serial and revokes older serials atomically on the control plane.
    pub async fn activate_certificate(&self) -> Result<(), TransportError> {
        let response = self.inner.certificate_activation_request()?.send().await?;
        accepted_status(response, reqwest::StatusCode::NO_CONTENT).await?;
        Ok(())
    }

    /// Performs a read-only authenticated round trip before a locally
    /// recovered enrollment is reported as usable. This proves that the
    /// exact Node/serial is still active in the control-plane ledger without
    /// claiming a Job or activating/revoking any certificate.
    pub async fn verify_identity(
        &self,
        expected_node_id: &str,
        expected_serial_hex: &str,
    ) -> Result<(), TransportError> {
        if expected_node_id.trim().is_empty()
            || expected_node_id.contains('/')
            || expected_serial_hex.is_empty()
            || expected_serial_hex.len() > 128
            || !expected_serial_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TransportError::Configuration(
                "identity verification requires a path-safe Node ID and certificate serial"
                    .to_string(),
            ));
        }
        let url = self
            .inner
            .endpoint(&["nodes", expected_node_id, "identity"])?;
        let response =
            accepted_status(self.inner.get(url).send().await?, reqwest::StatusCode::OK).await?;
        if response
            .content_length()
            .is_some_and(|length| length > 4_096)
        {
            return Err(TransportError::Protocol(
                "identity verification response exceeds protocol bound".to_string(),
            ));
        }
        let bytes = response.bytes().await?;
        if bytes.len() > 4_096 {
            return Err(TransportError::Protocol(
                "identity verification response exceeds protocol bound".to_string(),
            ));
        }
        let proof: IdentityVerificationResponse = serde_json::from_slice(&bytes)
            .map_err(|error| TransportError::Protocol(error.to_string()))?;
        validate_identity_verification(&proof, expected_node_id, expected_serial_hex)
    }

    pub fn artifact_fetcher(
        &self,
        node_id: impl Into<String>,
    ) -> Result<HttpArtifactFetcher, TransportError> {
        HttpArtifactFetcher::new(self.inner.clone(), node_id.into())
    }

    pub fn workload_credential_exchanger(
        &self,
        node_id: impl Into<String>,
    ) -> Result<HttpWorkloadCredentialExchanger, TransportError> {
        HttpWorkloadCredentialExchanger::new(self.inner.clone(), node_id.into())
    }

    pub fn runtime_facts_publisher(
        &self,
        node_id: impl Into<String>,
    ) -> Result<HttpNodeRuntimeFactsPublisher, TransportError> {
        HttpNodeRuntimeFactsPublisher::new(self.inner.clone(), node_id.into())
    }
}

impl EnrollmentClient {
    pub fn from_ca_pem(
        control_plane: &str,
        server_ca_bundle_pem: &[u8],
    ) -> Result<Self, TransportError> {
        let mut base_url = secure_base_url(control_plane)?;
        let roots = Certificate::from_pem_bundle(server_ca_bundle_pem)?;
        if roots.is_empty() {
            return Err(TransportError::Configuration(
                "the server CA bundle did not contain a certificate".to_string(),
            ));
        }
        base_url.set_path("/");
        let client = Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .tls_certs_only(roots)
            .build()?;
        Ok(Self {
            inner: ProtocolHttpTransport {
                base_url,
                client,
                local_bootstrap_secret: None,
            },
        })
    }

    pub async fn redeem(
        &self,
        enrollment_code: &str,
        csr_pem: &str,
    ) -> Result<NodeCertificateBundle, TransportError> {
        if enrollment_code.trim().is_empty() {
            return Err(TransportError::Configuration(
                "enrollment code is required".to_string(),
            ));
        }
        if csr_pem.is_empty() || csr_pem.len() > 64 * 1024 {
            return Err(TransportError::Configuration(
                "CSR must contain 1-65536 bytes".to_string(),
            ));
        }
        let url = self.inner.endpoint(&["enroll"])?;
        let response = self
            .inner
            .post(url)
            .json(&EnrollmentRequest {
                enrollment_code,
                csr_pem,
            })
            .send()
            .await?;
        accepted_status(response, reqwest::StatusCode::CREATED)
            .await?
            .json()
            .await
            .map_err(Into::into)
    }
}

impl LoopbackHttpTransport {
    /// Creates the Desktop-only plaintext transport. Only literal IPv4/IPv6
    /// loopback origins are accepted, and redirects are disabled so a local
    /// server cannot redirect protocol requests to another host.
    pub fn new(control_plane: &str) -> Result<Self, TransportError> {
        Self::build(control_plane, None)
    }

    pub fn new_with_bootstrap(
        control_plane: &str,
        bootstrap_secret: impl Into<String>,
    ) -> Result<Self, TransportError> {
        let secret = bootstrap_secret.into();
        if secret.trim().is_empty() {
            return Err(TransportError::Configuration(
                "Desktop Agent bootstrap secret is required".to_string(),
            ));
        }
        Self::build(control_plane, Some(secret))
    }

    fn build(
        control_plane: &str,
        local_bootstrap_secret: Option<String>,
    ) -> Result<Self, TransportError> {
        let mut base_url = Url::parse(control_plane)
            .map_err(|error| TransportError::Configuration(error.to_string()))?;
        if base_url.scheme() != "http" {
            return Err(TransportError::Configuration(
                "Desktop loopback transport requires an http URL".to_string(),
            ));
        }
        if base_url.cannot_be_a_base()
            || base_url.username() != ""
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || !matches!(base_url.path(), "" | "/")
        {
            return Err(TransportError::Configuration(
                "Desktop loopback URL must be a credential-free origin URL".to_string(),
            ));
        }
        let address = base_url
            .host_str()
            .and_then(|host| {
                host.strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host)
                    .parse::<IpAddr>()
                    .ok()
            })
            .filter(IpAddr::is_loopback)
            .ok_or_else(|| {
                TransportError::Configuration(
                    "Desktop plaintext transport requires a literal loopback IP address"
                        .to_string(),
                )
            })?;
        let _ = address;
        base_url.set_path("/");
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            inner: ProtocolHttpTransport {
                base_url,
                client,
                local_bootstrap_secret,
            },
        })
    }

    pub fn artifact_fetcher(
        &self,
        node_id: impl Into<String>,
    ) -> Result<HttpArtifactFetcher, TransportError> {
        HttpArtifactFetcher::new(self.inner.clone(), node_id.into())
    }

    pub fn workload_credential_exchanger(
        &self,
        node_id: impl Into<String>,
    ) -> Result<HttpWorkloadCredentialExchanger, TransportError> {
        HttpWorkloadCredentialExchanger::new(self.inner.clone(), node_id.into())
    }

    pub fn runtime_facts_publisher(
        &self,
        node_id: impl Into<String>,
    ) -> Result<HttpNodeRuntimeFactsPublisher, TransportError> {
        HttpNodeRuntimeFactsPublisher::new(self.inner.clone(), node_id.into())
    }
}

impl ProtocolHttpTransport {
    fn endpoint(&self, segments: &[&str]) -> Result<Url, TransportError> {
        let mut url = self.base_url.clone();
        let mut path = url.path_segments_mut().map_err(|_| {
            TransportError::Configuration("control-plane URL cannot contain path segments".into())
        })?;
        path.pop_if_empty();
        path.extend(["api", "v1", "agent"]);
        path.extend(segments.iter().copied());
        drop(path);
        Ok(url)
    }

    fn post(&self, url: Url) -> reqwest::RequestBuilder {
        let request = self.client.post(url);
        match self.local_bootstrap_secret.as_deref() {
            Some(secret) => request.header("x-ojos-agent-bootstrap", secret),
            None => request,
        }
    }

    fn certificate_activation_request(&self) -> Result<reqwest::RequestBuilder, TransportError> {
        let url = self.endpoint(&["certificates:activate"])?;
        // The server's mutation middleware intentionally requires JSON even
        // for operations with no user-supplied fields. Sending an explicit
        // empty object gives the request both an unambiguous JSON body and the
        // application/json Content-Type which protects the mutation boundary.
        Ok(self.post(url).json(&serde_json::json!({})))
    }

    fn get(&self, url: Url) -> reqwest::RequestBuilder {
        let request = self.client.get(url);
        match self.local_bootstrap_secret.as_deref() {
            Some(secret) => request.header("x-ojos-agent-bootstrap", secret),
            None => request,
        }
    }

    fn put(&self, url: Url) -> reqwest::RequestBuilder {
        let request = self.client.put(url);
        match self.local_bootstrap_secret.as_deref() {
            Some(secret) => request.header("x-ojos-agent-bootstrap", secret),
            None => request,
        }
    }
}

impl HttpArtifactFetcher {
    fn new(inner: ProtocolHttpTransport, node_id: String) -> Result<Self, TransportError> {
        if node_id.trim().is_empty() || node_id.contains('/') {
            return Err(TransportError::Configuration(
                "artifact fetcher requires a path-safe node_id".to_string(),
            ));
        }
        Ok(Self { inner, node_id })
    }
}

impl HttpWorkloadCredentialExchanger {
    fn new(inner: ProtocolHttpTransport, node_id: String) -> Result<Self, TransportError> {
        if node_id.trim().is_empty() || node_id.contains('/') {
            return Err(TransportError::Configuration(
                "workload credential exchanger requires a path-safe node_id".to_string(),
            ));
        }
        Ok(Self { inner, node_id })
    }
}

impl HttpNodeRuntimeFactsPublisher {
    fn new(inner: ProtocolHttpTransport, node_id: String) -> Result<Self, TransportError> {
        if node_id.trim().is_empty() || node_id.contains('/') {
            return Err(TransportError::Configuration(
                "runtime facts publisher requires a path-safe node_id".to_string(),
            ));
        }
        Ok(Self { inner, node_id })
    }
}

#[async_trait]
impl NodeRuntimeFactsPublisher for HttpNodeRuntimeFactsPublisher {
    async fn publish_runtime_facts(
        &self,
        node_id: &str,
        facts: &NodeRuntimeFactsV1,
    ) -> Result<(), RuntimePolicyError> {
        if node_id != self.node_id
            || facts.schema_version != 1
            || facts.report_id.trim().is_empty()
            || facts.observed_at_ms <= 0
            || facts.agent_version.trim().is_empty()
            || facts.runtime_policy_sha256.len() != 71
        {
            return Err(RuntimePolicyError::Publication(
                "runtime facts do not match the authenticated Node or protocol v1 bounds"
                    .to_string(),
            ));
        }
        let url = self
            .inner
            .endpoint(&["nodes", &self.node_id, "runtime-facts"])
            .map_err(|error| RuntimePolicyError::Publication(error.to_string()))?;
        let response = self
            .inner
            .put(url)
            .json(facts)
            .send()
            .await
            .map_err(|error| RuntimePolicyError::Publication(error.to_string()))?;
        accepted_status(response, reqwest::StatusCode::NO_CONTENT)
            .await
            .map_err(|error| RuntimePolicyError::Publication(error.to_string()))?;
        Ok(())
    }
}

#[derive(Serialize)]
struct WorkloadCredentialExchangeBody<'a> {
    deployment_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_token: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadCredentialExchangeResponse {
    access_token: String,
    token_type: String,
    expires_at_ms: i64,
    expires_in: u64,
}

#[async_trait]
impl WorkloadCredentialExchanger for HttpWorkloadCredentialExchanger {
    async fn exchange_workload_credential(
        &self,
        request: WorkloadCredentialExchangeRequest<'_>,
    ) -> Result<WorkloadCredential, RuntimePolicyError> {
        let has_job = request.job_id.is_some();
        let has_lease = request.lease_token.is_some();
        if request.deployment_id.trim().is_empty()
            || request.deployment_id.len() > 256
            || has_job != has_lease
            || request.job_id.is_some_and(str::is_empty)
            || request.lease_token.is_some_and(str::is_empty)
        {
            return Err(RuntimePolicyError::Credential(
                "credential exchange requires deployment_id and either both or neither job_id/lease_token"
                    .to_string(),
            ));
        }
        let url = self
            .inner
            .endpoint(&["nodes", &self.node_id, "workload-credentials:exchange"])
            .map_err(|error| RuntimePolicyError::Credential(error.to_string()))?;
        let response = self
            .inner
            .post(url)
            .json(&WorkloadCredentialExchangeBody {
                deployment_id: request.deployment_id,
                job_id: request.job_id,
                lease_token: request.lease_token,
            })
            .send()
            .await
            .map_err(|error| RuntimePolicyError::Credential(error.to_string()))?;
        if response.status() != reqwest::StatusCode::OK {
            // A credential endpoint must never copy a response body into an
            // error that can reach Agent logs. Even a misbehaving control
            // plane therefore cannot disclose an access token here.
            return Err(RuntimePolicyError::Credential(format!(
                "credential exchange rejected with HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > 32 * 1024)
        {
            return Err(RuntimePolicyError::Credential(
                "credential exchange response exceeds 32 KiB".to_string(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| RuntimePolicyError::Credential(error.to_string()))?;
        if bytes.len() > 32 * 1024 {
            return Err(RuntimePolicyError::Credential(
                "credential exchange response exceeds 32 KiB".to_string(),
            ));
        }
        let response: WorkloadCredentialExchangeResponse = serde_json::from_slice(&bytes)
            .map_err(|error| RuntimePolicyError::Credential(error.to_string()))?;
        if response.token_type != "Bearer" || response.expires_in != 15 * 60 {
            return Err(RuntimePolicyError::Credential(
                "credential exchange must return token_type=Bearer and expires_in=900".to_string(),
            ));
        }
        Ok(WorkloadCredential {
            access_token: response.access_token,
            expires_at_ms: response.expires_at_ms,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactChunkResponse {
    artifact_id: String,
    sha256: String,
    offset: u64,
    total_size: u64,
    data_base64: String,
    eof: bool,
}

#[async_trait]
impl ArtifactFetcher for HttpArtifactFetcher {
    async fn download(
        &self,
        job: &LeasedJob,
        reference: &ArtifactReference,
    ) -> Result<DownloadedArtifact, TransportError> {
        if reference.size_bytes == 0
            || reference.size_bytes > 512 * 1024 * 1024
            || reference.chunk_bytes == 0
            || reference.chunk_bytes > 2 * 1024 * 1024
        {
            return Err(TransportError::Protocol(
                "artifact reference exceeds protocol v1 bounds".to_string(),
            ));
        }
        let mut file = NamedTempFile::new()?;
        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        while offset < reference.size_bytes {
            let mut url = self.inner.endpoint(&[
                "nodes",
                &self.node_id,
                "jobs",
                &job.job_id,
                "artifacts",
                &reference.artifact_id,
            ])?;
            url.query_pairs_mut()
                .append_pair("offset", &offset.to_string())
                .append_pair("length", &reference.chunk_bytes.to_string());
            let response = self
                .inner
                .get(url)
                .header("x-ojos-lease-token", &job.lease_token)
                .send()
                .await?;
            if response
                .content_length()
                .is_some_and(|length| length > 3 * 1024 * 1024)
            {
                return Err(TransportError::Protocol(
                    "artifact chunk response exceeds protocol bound".to_string(),
                ));
            }
            let response = accepted_status(response, reqwest::StatusCode::OK).await?;
            let encoded = response.bytes().await?;
            if encoded.len() > 3 * 1024 * 1024 {
                return Err(TransportError::Protocol(
                    "artifact chunk response exceeds protocol bound".to_string(),
                ));
            }
            let chunk: ArtifactChunkResponse = serde_json::from_slice(&encoded)
                .map_err(|error| TransportError::Protocol(error.to_string()))?;
            if chunk.artifact_id != reference.artifact_id
                || chunk.sha256 != reference.sha256
                || chunk.offset != offset
                || chunk.total_size != reference.size_bytes
            {
                return Err(TransportError::Protocol(
                    "artifact chunk metadata does not match the Job reference".to_string(),
                ));
            }
            let bytes = BASE64_STANDARD.decode(chunk.data_base64).map_err(|error| {
                TransportError::Protocol(format!("invalid artifact chunk base64: {error}"))
            })?;
            if bytes.is_empty()
                || bytes.len() > reference.chunk_bytes as usize
                || offset.saturating_add(bytes.len() as u64) > reference.size_bytes
            {
                return Err(TransportError::Protocol(
                    "artifact chunk length is invalid".to_string(),
                ));
            }
            file.write_all(&bytes)?;
            hasher.update(&bytes);
            offset += bytes.len() as u64;
            if chunk.eof != (offset == reference.size_bytes) {
                return Err(TransportError::Protocol(
                    "artifact eof marker is inconsistent".to_string(),
                ));
            }
        }
        file.flush()?;
        let actual = format!("sha256:{:x}", hasher.finalize());
        if actual != reference.sha256 || offset != reference.size_bytes {
            return Err(TransportError::Protocol(
                "downloaded artifact checksum or size mismatch".to_string(),
            ));
        }
        Ok(DownloadedArtifact { file })
    }
}

#[derive(Serialize)]
struct ClaimBody<'a> {
    instance_id: &'a str,
    protocol_version: &'static str,
    capabilities: &'static [&'static str],
    max_jobs: u8,
}

const CAPABILITIES: &[&str] = &[
    "install",
    "release_pipeline",
    "upgrade",
    "start",
    "stop",
    "restart",
    "uninstall",
    "rollback",
    "health",
    "binding_context_apply",
    "resource_purge",
];

const CLAIM_PREFER: &str = "wait=25";

#[derive(Serialize)]
struct LeaseBody<'a> {
    lease_token: &'a str,
    events: &'a [NewJobEvent],
}

#[derive(Serialize)]
struct CompleteBody<'a> {
    lease_token: &'a str,
    status: &'a orchestrator_control_plane::CompletionStatus,
    result: &'a Value,
    error_message: &'a str,
    events: &'a [NewJobEvent],
}

#[derive(Serialize)]
struct EnrollmentRequest<'a> {
    enrollment_code: &'a str,
    csr_pem: &'a str,
}

#[derive(Serialize)]
struct CertificateRequest<'a> {
    csr_pem: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityVerificationResponse {
    node_id: String,
    spiffe_id: String,
    serial_hex: String,
    status: String,
}

fn validate_identity_verification(
    proof: &IdentityVerificationResponse,
    expected_node_id: &str,
    expected_serial_hex: &str,
) -> Result<(), TransportError> {
    if proof.node_id != expected_node_id
        || proof.spiffe_id != format!("spiffe://ojos.local/node/{expected_node_id}")
        || !proof.serial_hex.eq_ignore_ascii_case(expected_serial_hex)
        || proof.status != "ACTIVE"
    {
        return Err(TransportError::Protocol(
            "identity verification response does not match the recovered Node certificate"
                .to_string(),
        ));
    }
    Ok(())
}

fn secure_base_url(control_plane: &str) -> Result<Url, TransportError> {
    let mut base_url = Url::parse(control_plane)
        .map_err(|error| TransportError::Configuration(error.to_string()))?;
    if base_url.scheme() != "https" {
        return Err(TransportError::Configuration(
            "control plane must use https for Node identity operations".to_string(),
        ));
    }
    if base_url.cannot_be_a_base() || base_url.host_str().is_none() {
        return Err(TransportError::Configuration(
            "control plane must be an absolute HTTPS URL".to_string(),
        ));
    }
    base_url.set_query(None);
    base_url.set_fragment(None);
    Ok(base_url)
}

#[async_trait]
impl AgentTransport for ProtocolHttpTransport {
    async fn claim(&self, request: AgentClaimRequest) -> Result<ClaimResponse, TransportError> {
        let url = self.endpoint(&["nodes", &request.node_id, "jobs:claim"])?;
        let response = self
            .post(url)
            .header("Prefer", CLAIM_PREFER)
            .json(&ClaimBody {
                instance_id: &request.instance_id,
                protocol_version: "v1",
                capabilities: CAPABILITIES,
                max_jobs: 1,
            })
            .send()
            .await?;
        let response = accepted_status(response, reqwest::StatusCode::OK).await?;
        let claimed: ClaimResponse = response.json().await?;
        if claimed.jobs.len() > 1 {
            return Err(TransportError::Protocol(format!(
                "claim returned {} jobs; protocol v1 permits at most one",
                claimed.jobs.len()
            )));
        }
        Ok(claimed)
    }

    async fn heartbeat(
        &self,
        node_id: &str,
        request: HeartbeatRequest,
    ) -> Result<HeartbeatAck, TransportError> {
        let job_action = format!("{}:heartbeat", request.job_id);
        let url = self.endpoint(&["nodes", node_id, "jobs", &job_action])?;
        let response = self
            .post(url)
            .json(&LeaseBody {
                lease_token: &request.lease_token,
                events: &request.events,
            })
            .send()
            .await?;
        let response = accepted_status(response, reqwest::StatusCode::OK).await?;
        let status = response.status();
        let content_length = response.content_length();
        let bytes = response.bytes().await?;
        decode_heartbeat_ack(status, content_length, &bytes)
    }

    async fn complete(
        &self,
        node_id: &str,
        request: CompleteRequest,
    ) -> Result<(), TransportError> {
        let job_action = format!("{}:complete", request.job_id);
        let url = self.endpoint(&["nodes", node_id, "jobs", &job_action])?;
        let response = self
            .post(url)
            .json(&CompleteBody {
                lease_token: &request.lease_token,
                status: &request.status,
                result: &request.result,
                error_message: &request.error_message,
                events: &request.events,
            })
            .send()
            .await?;
        accepted_status(response, reqwest::StatusCode::NO_CONTENT).await?;
        Ok(())
    }
}

fn decode_heartbeat_ack(
    status: reqwest::StatusCode,
    content_length: Option<u64>,
    bytes: &[u8],
) -> Result<HeartbeatAck, TransportError> {
    if status == reqwest::StatusCode::NO_CONTENT || content_length == Some(0) || bytes.is_empty() {
        return Err(TransportError::Protocol(
            "heartbeat response must contain the protocol v1 acknowledgement object".to_string(),
        ));
    }
    serde_json::from_slice(bytes).map_err(|error| TransportError::Protocol(error.to_string()))
}

macro_rules! forward_transport {
    ($transport:ty) => {
        #[async_trait]
        impl AgentTransport for $transport {
            async fn claim(
                &self,
                request: AgentClaimRequest,
            ) -> Result<ClaimResponse, TransportError> {
                self.inner.claim(request).await
            }

            async fn heartbeat(
                &self,
                node_id: &str,
                request: HeartbeatRequest,
            ) -> Result<HeartbeatAck, TransportError> {
                self.inner.heartbeat(node_id, request).await
            }

            async fn complete(
                &self,
                node_id: &str,
                request: CompleteRequest,
            ) -> Result<(), TransportError> {
                self.inner.complete(node_id, request).await
            }
        }
    };
}

forward_transport!(HttpMtlsTransport);
forward_transport!(LoopbackHttpTransport);

async fn accepted_status(
    response: Response,
    expected: reqwest::StatusCode,
) -> Result<Response, TransportError> {
    if response.status() == expected {
        return Ok(response);
    }
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Err(TransportError::Protocol(format!(
            "agent protocol expected HTTP {} but received {status}",
            expected.as_u16()
        )));
    }
    let mut body = response.text().await.unwrap_or_default();
    body.truncate(4_096);
    Err(TransportError::Rejected { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Value as YamlValue;

    #[test]
    fn plaintext_control_plane_is_rejected_before_tls_parsing() {
        let result = HttpMtlsTransport::from_pem("http://127.0.0.1:8080", b"", b"", b"");
        assert!(matches!(result, Err(TransportError::Configuration(_))));
    }

    #[test]
    fn loopback_transport_accepts_only_literal_plaintext_loopback_origins() {
        assert!(LoopbackHttpTransport::new("http://127.0.0.1:38123/").is_ok());
        assert!(LoopbackHttpTransport::new("http://[::1]:38123/").is_ok());
        for rejected in [
            "https://127.0.0.1:38123/",
            "http://localhost:38123/",
            "http://192.0.2.10:38123/",
            "http://example.test/",
            "http://127.0.0.1.example.test/",
            "http://user@127.0.0.1:38123/",
            "http://127.0.0.1:38123/path",
            "http://127.0.0.1:38123/?redirect=remote",
        ] {
            assert!(
                matches!(
                    LoopbackHttpTransport::new(rejected),
                    Err(TransportError::Configuration(_))
                ),
                "{rejected}"
            );
        }
    }

    #[test]
    fn certificate_activation_sends_an_explicit_empty_json_object() {
        let transport =
            LoopbackHttpTransport::new("http://127.0.0.1:38123/").expect("loopback transport");
        let request = transport
            .inner
            .certificate_activation_request()
            .expect("activation request")
            .build()
            .expect("build activation request");

        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.url().path(), "/api/v1/agent/certificates:activate");
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            request.body().and_then(reqwest::Body::as_bytes),
            Some(b"{}".as_slice())
        );
    }

    #[test]
    fn identity_verification_requires_the_exact_active_node_and_serial() {
        let valid = IdentityVerificationResponse {
            node_id: "node-1".to_string(),
            spiffe_id: "spiffe://ojos.local/node/node-1".to_string(),
            serial_hex: "0a".to_string(),
            status: "ACTIVE".to_string(),
        };
        validate_identity_verification(&valid, "node-1", "0A").unwrap();

        for invalid in [
            IdentityVerificationResponse {
                node_id: "node-2".to_string(),
                ..valid.clone()
            },
            IdentityVerificationResponse {
                serial_hex: "0b".to_string(),
                ..valid.clone()
            },
            IdentityVerificationResponse {
                status: "REVOKED".to_string(),
                ..valid.clone()
            },
        ] {
            assert!(validate_identity_verification(&invalid, "node-1", "0a").is_err());
        }
    }

    #[test]
    fn checked_in_protocol_exactly_matches_claimed_job_kinds_and_capabilities() {
        let protocol: YamlValue = serde_yaml::from_str(include_str!(
            "../../../../platform/schemas/orchestrator/agent-protocol-v1.yaml"
        ))
        .expect("valid checked-in Agent protocol");
        let declared = protocol["components"]["schemas"]["ClaimResponse"]["properties"]["jobs"]
            ["items"]["properties"]["kind"]["enum"]
            .as_sequence()
            .expect("ClaimResponse Job kind enum")
            .iter()
            .map(|value| value.as_str().expect("string Job kind"))
            .collect::<Vec<_>>();
        let compiled = [
            JobKind::Install,
            JobKind::ReleasePipeline,
            JobKind::Upgrade,
            JobKind::Start,
            JobKind::Stop,
            JobKind::Restart,
            JobKind::Uninstall,
            JobKind::Rollback,
            JobKind::Health,
            JobKind::BindingContextApply,
            JobKind::ResourcePurge,
        ]
        .iter()
        .map(|kind| {
            serde_json::to_value(kind)
                .expect("serialize Job kind")
                .as_str()
                .expect("string Job kind")
                .to_string()
        })
        .collect::<Vec<_>>();
        assert_eq!(
            declared,
            compiled.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(CAPABILITIES, compiled);

        let claim_schema = &protocol["paths"]["/nodes/{nodeId}/jobs:claim"]["post"]["requestBody"]
            ["content"]["application/json"]["schema"];
        let required = claim_schema["required"]
            .as_sequence()
            .expect("claim required fields")
            .iter()
            .map(|value| value.as_str().expect("string required field"))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(required.contains("max_jobs"));
        let prefer = protocol["paths"]["/nodes/{nodeId}/jobs:claim"]["post"]["parameters"]
            .as_sequence()
            .expect("claim parameters")
            .iter()
            .find(|parameter| parameter["name"].as_str() == Some("Prefer"))
            .expect("claim Prefer parameter");
        assert_eq!(prefer["schema"]["const"].as_str(), Some(CLAIM_PREFER));
        let advertised = claim_schema["properties"]["capabilities"]["items"]["enum"]
            .as_sequence()
            .expect("claim capability enum")
            .iter()
            .map(|value| value.as_str().expect("string capability"))
            .collect::<Vec<_>>();
        assert_eq!(advertised, CAPABILITIES);

        for schema_name in [
            "CertificateResponse",
            "ClaimResponse",
            "HeartbeatAck",
            "ArtifactChunk",
            "WorkloadCredentialExchangeResponse",
        ] {
            let schema = &protocol["components"]["schemas"][schema_name];
            assert_eq!(
                schema["additionalProperties"].as_bool(),
                Some(false),
                "{schema_name} must remain closed"
            );
            assert!(
                schema["properties"]["status"].is_null()
                    && !schema["required"]
                        .as_sequence()
                        .expect("required fields")
                        .iter()
                        .any(|field| field.as_str() == Some("status")),
                "{schema_name} must not inherit the legacy public API status field"
            );
        }

        assert!(
            protocol["paths"]["/nodes/{nodeId}/jobs/{jobId}/artifacts/{artifactId}"]["get"]
                .is_mapping()
        );
        assert!(protocol["paths"]["/nodes/{nodeId}/runtime-facts"]["put"].is_mapping());
        assert!(
            protocol["paths"]["/nodes/{nodeId}/workload-credentials:exchange"]["post"].is_mapping()
        );
        assert_eq!(
            protocol["components"]["schemas"]["RuntimeContract"]["allOf"][0]["if"]
                ["properties"]["id"]["const"]
                .as_str(),
            Some(orchestrator_runtime::STANDARD_RUNTIME_PROFILE_ID)
        );
        assert!(protocol["paths"]["/enroll"]["post"].is_mapping());
        assert!(protocol["paths"]["/certificates:renew"]["post"].is_mapping());
        assert!(protocol["paths"]["/certificates:activate"]["post"].is_mapping());
        let activation_body = &protocol["paths"]["/certificates:activate"]["post"]["requestBody"];
        assert_eq!(activation_body["required"].as_bool(), Some(true));
        let activation_schema = &activation_body["content"]["application/json"]["schema"];
        assert_eq!(activation_schema["type"].as_str(), Some("object"));
        assert_eq!(
            activation_schema["additionalProperties"].as_bool(),
            Some(false)
        );
        assert_eq!(activation_schema["maxProperties"].as_u64(), Some(0));
        let complete_responses =
            &protocol["paths"]["/nodes/{nodeId}/jobs/{jobId}:complete"]["post"]["responses"];
        assert!(complete_responses["204"].is_mapping());
        assert!(complete_responses["200"].is_null());
    }

    #[test]
    fn workload_credential_response_is_strict_and_debug_never_exposes_lease() {
        let response: WorkloadCredentialExchangeResponse =
            serde_json::from_value(serde_json::json!({
                "access_token": "secret-token",
                "token_type": "Bearer",
                "expires_at_ms": 123,
                "expires_in": 900,
            }))
            .unwrap();
        assert_eq!(response.expires_in, 900);
        assert!(
            serde_json::from_value::<WorkloadCredentialExchangeResponse>(serde_json::json!({
                "access_token": "secret-token",
                "token_type": "Bearer",
                "expires_at_ms": 123,
                "expires_in": 900,
                "status": "ok",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<WorkloadCredentialExchangeResponse>(serde_json::json!({
                "access_token": "secret-token",
                "token_type": "Bearer",
                "expires_at_ms": 123,
                "expires_in": 900,
                "unexpected": true,
            }))
            .is_err()
        );
        let request = WorkloadCredentialExchangeRequest {
            deployment_id: "deployment-1",
            job_id: Some("job-1"),
            lease_token: Some("lease-secret"),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("lease-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn all_agent_object_success_responses_reject_legacy_envelope_fields() {
        let claim: ClaimResponse =
            serde_json::from_value(serde_json::json!({"jobs": [], "retry_after_ms": 0})).unwrap();
        assert!(claim.jobs.is_empty());
        assert!(
            serde_json::from_value::<ClaimResponse>(
                serde_json::json!({"jobs": [], "retry_after_ms": 0, "status": "ok"}),
            )
            .is_err()
        );

        let heartbeat: HeartbeatAck =
            serde_json::from_value(serde_json::json!({"cancel_requested": false})).unwrap();
        assert!(!heartbeat.cancel_requested);
        assert!(
            serde_json::from_value::<HeartbeatAck>(serde_json::json!({
                "cancel_requested": false,
                "request_id": "not-part-of-agent-protocol-v1"
            }))
            .is_err()
        );
    }

    #[test]
    fn heartbeat_requires_the_exact_non_empty_200_acknowledgement() {
        assert_eq!(
            decode_heartbeat_ack(
                reqwest::StatusCode::OK,
                Some(26),
                br#"{"cancel_requested":false}"#,
            )
            .unwrap(),
            HeartbeatAck {
                cancel_requested: false
            }
        );
        for (status, length, body) in [
            (reqwest::StatusCode::NO_CONTENT, Some(0), b"".as_slice()),
            (reqwest::StatusCode::OK, Some(0), b"".as_slice()),
            (reqwest::StatusCode::OK, None, b"".as_slice()),
        ] {
            assert!(decode_heartbeat_ack(status, length, body).is_err());
        }
        assert!(
            decode_heartbeat_ack(
                reqwest::StatusCode::OK,
                None,
                br#"{"cancel_requested":false,"status":"ok"}"#,
            )
            .is_err()
        );
    }
}
