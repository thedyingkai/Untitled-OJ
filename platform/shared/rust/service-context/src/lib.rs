use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt;
use opentelemetry::Context;
use opentelemetry::global;
use opentelemetry::propagation::Injector;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Certificate, Client, ClientBuilder, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

mod provider;
pub use provider::{
    BindingUnavailable, ContextProvider, ContextUpdate, DEFAULT_CONTEXT_POLL_INTERVAL,
};

pub const DEFAULT_SERVICE_CONTEXT_FILE: &str = "/run/ojos/service/context.json";
const MAX_CONTEXT_BYTES: u64 = 1024 * 1024;
const MAX_CREDENTIAL_BYTES: u64 = 16 * 1024;
const DOWNLOAD_MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadRetry {
    Never,
    Transient,
    RotatedCredential,
}

#[derive(Debug)]
struct DownloadAttemptFailure {
    error: anyhow::Error,
    retry: DownloadRetry,
}

impl DownloadAttemptFailure {
    fn retryable(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            retry: DownloadRetry::Transient,
        }
    }

    fn fatal(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            retry: DownloadRetry::Never,
        }
    }

    fn rotated_credential(error: impl Into<anyhow::Error>) -> Self {
        Self {
            error: error.into(),
            retry: DownloadRetry::RotatedCredential,
        }
    }
}

struct WorkloadCredential {
    value: String,
    fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceContext {
    pub schema_version: u32,
    pub deployment: DeploymentIdentity,
    pub gateway: GatewayContext,
    pub bindings: BTreeMap<String, ApiBinding>,
    pub credential_file: PathBuf,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentIdentity {
    pub id: String,
    pub service: String,
    pub node: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayContext {
    pub origin: String,
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiBinding {
    pub binding_id: String,
    pub api_id: String,
    pub base_path: String,
    pub timeout_ms: u64,
}

impl ServiceContext {
    pub fn load_optional() -> Result<Option<Self>> {
        let explicit = std::env::var_os("OJOS_SERVICE_CONTEXT_FILE").map(PathBuf::from);
        let path = explicit
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SERVICE_CONTEXT_FILE));
        if !path.exists() {
            if explicit.is_some() || managed_workload_required() {
                return Err(anyhow!(
                    "service context file is required but missing: {}",
                    path.display()
                ));
            }
            return Ok(None);
        }
        Self::load(&path).map(Some)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("inspect service context failed: {}", path.display()))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CONTEXT_BYTES {
            return Err(anyhow!("service context must be a bounded regular file"));
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("read service context failed: {}", path.display()))?;
        let context: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("decode service context failed: {}", path.display()))?;
        context.validate()?;
        Ok(context)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(anyhow!(
                "unsupported service context schema version {}",
                self.schema_version
            ));
        }
        for (label, value) in [
            ("deployment.id", self.deployment.id.as_str()),
            ("deployment.service", self.deployment.service.as_str()),
            ("deployment.node", self.deployment.node.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(anyhow!("service context {label} is required"));
            }
        }
        let origin = reqwest::Url::parse(&self.gateway.origin)
            .map_err(|error| anyhow!("gateway origin is invalid: {error}"))?;
        if self.gateway.origin != self.gateway.origin.trim()
            || self.gateway.origin.ends_with('/')
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.host_str().is_none()
        {
            return Err(anyhow!(
                "gateway origin must be a scheme+authority without userinfo, path, query or fragment"
            ));
        }
        if !(origin.scheme() == "https" || development_http_allowed(&origin)) {
            return Err(anyhow!(
                "gateway origin must use https outside explicit development mode"
            ));
        }
        if !self.credential_file.is_absolute() {
            return Err(anyhow!("service context credential_file must be absolute"));
        }
        if self
            .gateway
            .ca_file
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err(anyhow!("service context gateway.ca_file must be absolute"));
        }
        for (name, binding) in &self.bindings {
            if name.trim().is_empty()
                || binding.binding_id.trim().is_empty()
                || binding.api_id.trim().is_empty()
                || binding.timeout_ms == 0
                || binding.timeout_ms > 300_000
            {
                return Err(anyhow!("service context binding {name:?} is incomplete"));
            }
            let expected = format!("/internal/apis/{}", binding.api_id);
            if binding.base_path.trim_end_matches('/') != expected {
                return Err(anyhow!(
                    "binding {name} base_path must be {expected}, got {}",
                    binding.base_path
                ));
            }
        }
        Ok(())
    }

    pub fn require_service(&self, expected: &str) -> Result<()> {
        if self.deployment.service != expected {
            return Err(anyhow!(
                "service context belongs to {}, expected {}",
                self.deployment.service,
                expected
            ));
        }
        Ok(())
    }

    pub fn binding(&self, name: &str) -> Result<&ApiBinding> {
        self.bindings
            .get(name)
            .ok_or_else(|| anyhow!("required API binding {name:?} is missing"))
    }

    pub fn binding_url(&self, name: &str, relative_path: &str) -> Result<String> {
        let binding = self.binding(name)?;
        Ok(format!(
            "{}{}{}",
            self.gateway.origin.trim_end_matches('/'),
            binding.base_path.trim_end_matches('/'),
            normalized_relative_path(relative_path)?
        ))
    }

    pub fn client(&self) -> Result<Client> {
        let timeout_ms = self
            .bindings
            .values()
            .map(|binding| binding.timeout_ms)
            .max()
            .unwrap_or(60_000)
            .max(35_000);
        let mut builder = ClientBuilder::new()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(path) = self.gateway.ca_file.as_deref() {
            let pem = std::fs::read(path)
                .with_context(|| format!("read gateway CA failed: {}", path.display()))?;
            let certificate = Certificate::from_pem(&pem)
                .with_context(|| format!("parse gateway CA failed: {}", path.display()))?;
            builder = builder.add_root_certificate(certificate);
        }
        builder
            .build()
            .context("create workload HTTP client failed")
    }

    async fn workload_credential(&self) -> Result<WorkloadCredential> {
        let metadata = fs::metadata(&self.credential_file)
            .await
            .with_context(|| "inspect workload credential failed")?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CREDENTIAL_BYTES {
            return Err(anyhow!("workload credential file is invalid"));
        }
        let token = fs::read_to_string(&self.credential_file)
            .await
            .with_context(|| "read workload credential failed")?;
        let token = token.trim();
        if token.is_empty() || token.chars().any(char::is_whitespace) {
            return Err(anyhow!("workload credential is invalid"));
        }
        Ok(WorkloadCredential {
            fingerprint: Sha256::digest(token.as_bytes()).into(),
            value: token.to_owned(),
        })
    }

    pub async fn authorize(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let credential = self.workload_credential().await?;
        Ok(request.bearer_auth(credential.value))
    }

    fn request_without_credential(
        &self,
        client: &Client,
        binding_name: &str,
        method: Method,
        relative_path: &str,
    ) -> Result<RequestBuilder> {
        let binding = self.binding(binding_name)?;
        let mut headers = HeaderMap::new();
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&Context::current(), &mut HeaderInjector(&mut headers));
        });
        if matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        ) {
            headers.insert(
                HeaderName::from_static("idempotency-key"),
                HeaderValue::from_str(&Uuid::new_v4().to_string())?,
            );
        }
        let request = client
            .request(method, self.binding_url(binding_name, relative_path)?)
            .headers(headers)
            .timeout(Duration::from_millis(binding.timeout_ms));
        Ok(request)
    }

    pub async fn request(
        &self,
        client: &Client,
        binding_name: &str,
        method: Method,
        relative_path: &str,
    ) -> Result<RequestBuilder> {
        let request =
            self.request_without_credential(client, binding_name, method, relative_path)?;
        self.authorize(request).await
    }

    async fn download_request(
        &self,
        client: &Client,
        binding_name: &str,
        relative_path: &str,
    ) -> Result<(RequestBuilder, [u8; 32])> {
        let request =
            self.request_without_credential(client, binding_name, Method::GET, relative_path)?;
        let credential = self.workload_credential().await?;
        Ok((
            request.bearer_auth(credential.value),
            credential.fingerprint,
        ))
    }

    pub async fn download_to(
        &self,
        client: &Client,
        binding_name: &str,
        relative_path: &str,
        expected_sha256: &str,
        expected_size: u64,
        target: &Path,
    ) -> Result<()> {
        let Some(expected_sha256) = expected_sha256_hex(expected_sha256) else {
            return Err(anyhow!("download artifact identity is invalid"));
        };
        if expected_size == 0 {
            return Err(anyhow!("download artifact identity is invalid"));
        }
        // Resolve static contract errors before entering the retry loop. Every
        // attempt below is a safe GET and reloads the rotated workload token.
        self.binding_url(binding_name, relative_path)?;
        let parent = target
            .parent()
            .ok_or_else(|| anyhow!("download target has no parent directory"))?;
        fs::create_dir_all(parent).await?;

        let mut credential_rotation_retried = false;
        for attempt in 0..DOWNLOAD_MAX_ATTEMPTS {
            let temporary = parent.join(format!(".ojos-download-{}.tmp", Uuid::new_v4()));
            let result = self
                .download_once(
                    client,
                    binding_name,
                    relative_path,
                    expected_sha256,
                    expected_size,
                    &temporary,
                    target,
                )
                .await;
            if result.is_ok() {
                return Ok(());
            }
            let _ = fs::remove_file(&temporary).await;
            let failure = result.expect_err("failed download attempt has an error");
            let retry = match failure.retry {
                DownloadRetry::Never => false,
                DownloadRetry::Transient => true,
                DownloadRetry::RotatedCredential if !credential_rotation_retried => {
                    credential_rotation_retried = true;
                    true
                }
                DownloadRetry::RotatedCredential => false,
            };
            if !retry || attempt + 1 == DOWNLOAD_MAX_ATTEMPTS {
                return Err(failure.error);
            }
            if failure.retry == DownloadRetry::Transient {
                tokio::time::sleep(download_retry_delay(attempt)).await;
            }
        }
        unreachable!("download attempt loop always returns")
    }

    #[allow(clippy::too_many_arguments)]
    async fn download_once(
        &self,
        client: &Client,
        binding_name: &str,
        relative_path: &str,
        expected_sha256: &str,
        expected_size: u64,
        temporary: &Path,
        target: &Path,
    ) -> std::result::Result<(), DownloadAttemptFailure> {
        let (request, request_credential_fingerprint) = self
            .download_request(client, binding_name, relative_path)
            .await
            .map_err(DownloadAttemptFailure::retryable)?;
        let response = request
            .send()
            .await
            .map_err(DownloadAttemptFailure::retryable)?;
        if !response.status().is_success() {
            let status = response.status();
            if matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) {
                let credential_rotated = self.workload_credential().await.is_ok_and(|current| {
                    !constant_time_digest_eq(&request_credential_fingerprint, &current.fingerprint)
                });
                let error = anyhow!("bound download returned {status}");
                return Err(if credential_rotated {
                    DownloadAttemptFailure::rotated_credential(error)
                } else {
                    DownloadAttemptFailure::fatal(error)
                });
            }
            let retryable = status == reqwest::StatusCode::REQUEST_TIMEOUT
                || status == reqwest::StatusCode::TOO_EARLY
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error();
            let error = anyhow!("bound download returned {status}");
            return Err(if retryable {
                DownloadAttemptFailure::retryable(error)
            } else {
                DownloadAttemptFailure::fatal(error)
            });
        }
        if response
            .content_length()
            .is_some_and(|actual| actual != expected_size)
        {
            return Err(DownloadAttemptFailure::fatal(anyhow!(
                "bound download size does not match resource reference"
            )));
        }

        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temporary)
            .await
            .map_err(|error| DownloadAttemptFailure::fatal(anyhow!(error)))?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut terminal_transport_error = None;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    terminal_transport_error = Some(error);
                    break;
                }
            };
            size = size.saturating_add(chunk.len() as u64);
            if size > expected_size {
                return Err(DownloadAttemptFailure::fatal(anyhow!(
                    "bound download exceeded declared size"
                )));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| DownloadAttemptFailure::fatal(anyhow!(error)))?;
        }
        file.flush()
            .await
            .map_err(|error| DownloadAttemptFailure::fatal(anyhow!(error)))?;

        if size != expected_size {
            let error = terminal_transport_error.map_or_else(
                || {
                    anyhow!(
                        "bound download size does not match resource reference: expected {expected_size}, received {size}"
                    )
                },
                |source| {
                    anyhow!(source).context(format!(
                        "bound download stream ended before declared size: expected {expected_size}, received {size}"
                    ))
                },
            );
            return Err(DownloadAttemptFailure::retryable(error));
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected_sha256 {
            return Err(DownloadAttemptFailure::fatal(anyhow!(
                "bound download SHA-256 does not match resource reference"
            )));
        }

        // rustls deliberately reports a TCP EOF without TLS close_notify.
        // Some proxies close exactly that way after forwarding a complete
        // chunked body. Never suppress the error on its own: the response is
        // publishable only after both the signed size and SHA-256 identity have
        // matched exactly. A truncated or altered stream returned above.
        drop(terminal_transport_error);
        fs::rename(temporary, target)
            .await
            .map_err(|error| DownloadAttemptFailure::fatal(anyhow!(error)))?;
        Ok(())
    }
}

struct HeaderInjector<'a>(&'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, value);
        }
    }
}

fn expected_sha256_hex(value: &str) -> Option<&str> {
    let value = value.trim().strip_prefix("sha256:").unwrap_or(value.trim());
    (value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    .then_some(value)
}

fn constant_time_digest_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn download_retry_delay(attempt: usize) -> Duration {
    match attempt {
        0 => Duration::from_millis(100),
        _ => Duration::from_millis(500),
    }
}

fn normalized_relative_path(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || trimmed.starts_with("//")
        || trimmed.contains('#')
        || trimmed.chars().any(char::is_control)
    {
        return Err(anyhow!("binding path must be relative to the selected API"));
    }
    let path = trimmed.split_once('?').map_or(trimmed, |(path, _)| path);
    for segment in path.split('/') {
        let segment = segment.to_ascii_lowercase();
        if matches!(
            segment.as_str(),
            "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
        ) || segment.contains("%2f")
            || segment.contains("%5c")
            || segment.contains('\\')
        {
            return Err(anyhow!(
                "binding path must not contain dot or encoded separator segments"
            ));
        }
    }
    Ok(format!("/{}", trimmed.trim_start_matches('/')))
}

fn managed_workload_required() -> bool {
    std::env::var("OJOS_MANAGED_WORKLOAD")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn development_http_allowed(origin: &reqwest::Url) -> bool {
    if origin.scheme() != "http" {
        return false;
    }
    std::env::var("OJOS_ALLOW_HTTP_SERVICE_CONTEXT")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || matches!(origin.host_str(), Some("127.0.0.1" | "localhost"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::JoinHandle;
    use std::time::Instant;

    fn context(root: &Path) -> ServiceContext {
        ServiceContext {
            schema_version: 1,
            deployment: DeploymentIdentity {
                id: "deployment-b".into(),
                service: "fixture-consumer".into(),
                node: "node-b".into(),
            },
            gateway: GatewayContext {
                origin: "http://127.0.0.1:8080".into(),
                ca_file: None,
            },
            bindings: BTreeMap::from([(
                "storage_get".into(),
                ApiBinding {
                    binding_id: "binding-1".into(),
                    api_id: "storage.object.get".into(),
                    base_path: "/internal/apis/storage.object.get".into(),
                    timeout_ms: 300_000,
                },
            )]),
            credential_file: root.join("token"),
            generation: 1,
        }
    }

    #[test]
    fn binding_url_is_generic_and_contains_no_credential() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        assert_eq!(
            context
                .binding_url("storage_get", "/submissions/42")
                .unwrap(),
            "http://127.0.0.1:8080/internal/apis/storage.object.get/submissions/42"
        );
        context.require_service("fixture-consumer").unwrap();
        assert!(context.require_service("judge-worker").is_err());
    }

    #[test]
    fn binding_path_cannot_escape_selected_api() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        for invalid in [
            "https://attacker.example/object",
            "//attacker.example/object",
            "/../admin",
            "/%2e%2e/admin",
            "/object%2f..%2fadmin",
        ] {
            assert!(
                context.binding_url("storage_get", invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn empty_optional_binding_set_is_valid_but_lookup_remains_explicit() {
        let root = tempfile::tempdir().unwrap();
        let mut context = context(root.path());
        context.gateway.origin = "https://gateway.example".into();
        context.bindings.clear();
        context.validate().unwrap();
        assert!(context.binding("storage_get").is_err());
    }

    #[test]
    fn gateway_origin_rejects_userinfo_and_non_origin_components() {
        let root = tempfile::tempdir().unwrap();
        let mut context = context(root.path());
        for origin in [
            "https://user@gateway.example",
            "https://gateway.example/",
            "https://gateway.example/path",
            "https://gateway.example?query=1",
            "https://gateway.example#fragment",
        ] {
            context.gateway.origin = origin.into();
            assert!(context.validate().is_err(), "{origin}");
        }
    }

    #[tokio::test]
    async fn credential_is_reloaded_for_each_request() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        std::fs::write(&context.credential_file, "first-token").unwrap();
        let client = context.client().unwrap();
        let first = context
            .request(&client, "storage_get", Method::GET, "/object")
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(first.headers()["authorization"], "Bearer first-token");
        std::fs::write(&context.credential_file, "second-token").unwrap();
        let second = context
            .request(&client, "storage_get", Method::GET, "/object")
            .await
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(second.headers()["authorization"], "Bearer second-token");
    }

    #[tokio::test]
    async fn download_retries_once_when_rejected_credential_was_rotated() {
        let root = tempfile::tempdir().unwrap();
        let artifact = b"artifact fetched after credential rotation".to_vec();
        let mut context = context(root.path());
        std::fs::write(&context.credential_file, "credential-before-rotation").unwrap();
        let credential_file = context.credential_file.clone();
        let response_artifact = artifact.clone();
        let server = ScriptedHttpServer::spawn(2, move |request_index, headers, stream| {
            if request_index == 0 {
                assert!(
                    bearer_token_from_request(headers)
                        .is_some_and(|token| token == "credential-before-rotation"),
                    "first request did not use the pre-rotation credential"
                );
                std::fs::write(&credential_file, "credential-after-rotation").unwrap();
                write!(
                    stream,
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            } else {
                assert!(
                    bearer_token_from_request(headers)
                        .is_some_and(|token| token == "credential-after-rotation"),
                    "retry did not reload the rotated credential"
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_artifact.len()
                )
                .unwrap();
                stream.write_all(&response_artifact).unwrap();
            }
        });
        context.gateway.origin = server.origin.clone();
        let client = context.client().unwrap();
        let target = root.path().join("artifact.bin");

        context
            .download_to(
                &client,
                "storage_get",
                "/objects/artifact",
                &format!("sha256:{:x}", Sha256::digest(&artifact)),
                artifact.len() as u64,
                &target,
            )
            .await
            .expect("a proven credential rotation permits one safe GET retry");

        server.finish(2);
        assert_eq!(fs::read(&target).await.unwrap(), artifact);
        assert_no_download_temporaries(root.path());
    }

    #[tokio::test]
    async fn download_does_not_retry_stable_forbidden_response() {
        let root = tempfile::tempdir().unwrap();
        let artifact = b"artifact that must remain unavailable".to_vec();
        let mut context = context(root.path());
        std::fs::write(&context.credential_file, "stable-forbidden-credential").unwrap();
        let server = ScriptedHttpServer::spawn(1, move |_request_index, headers, stream| {
            assert!(
                bearer_token_from_request(headers)
                    .is_some_and(|token| token == "stable-forbidden-credential"),
                "request did not use the expected stable credential"
            );
            write!(
                stream,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        context.gateway.origin = server.origin.clone();
        let client = context.client().unwrap();
        let target = root.path().join("artifact.bin");

        let error = context
            .download_to(
                &client,
                "storage_get",
                "/objects/artifact",
                &format!("sha256:{:x}", Sha256::digest(&artifact)),
                artifact.len() as u64,
                &target,
            )
            .await
            .expect_err("an unchanged credential cannot turn a permission denial into a retry");

        server.finish(1);
        assert!(error.to_string().contains("403 Forbidden"));
        assert!(!error.to_string().contains("stable-forbidden-credential"));
        assert!(!target.exists());
        assert_no_download_temporaries(root.path());
    }

    #[test]
    fn scripted_http_server_waits_for_headers_after_accept() {
        let server = ScriptedHttpServer::spawn(1, |_request_index, headers, stream| {
            assert!(headers.starts_with(b"GET /delayed HTTP/1.1\r\n"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let address = server
            .origin
            .strip_prefix("http://")
            .expect("scripted HTTP origin");
        let mut client = std::net::TcpStream::connect(address).unwrap();

        // Windows can propagate the listener's nonblocking mode to the
        // accepted socket. Delay the headers so this test catches an
        // immediate WouldBlock read instead of relying on scheduler timing.
        std::thread::sleep(Duration::from_millis(50));
        client
            .write_all(b"GET /delayed HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        server.finish(1);
    }

    #[tokio::test]
    async fn https_fixture_reproduces_full_body_followed_by_rustls_unexpected_eof() {
        let root = tempfile::tempdir().unwrap();
        let artifact = b"complete bytes before a broken TLS shutdown".to_vec();
        let server = AbruptHttpsServer::spawn(vec![artifact.clone()]);
        let context = tls_context(root.path(), &server);
        std::fs::write(&context.credential_file, "deployment-token").unwrap();
        let client = context.client().unwrap();
        let response = context
            .request(&client, "storage_get", Method::GET, "/objects/artifact")
            .await
            .unwrap()
            .send()
            .await
            .unwrap();
        let mut stream = response.bytes_stream();
        let mut received = Vec::new();
        let mut terminal_error = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => received.extend_from_slice(&chunk),
                Err(error) => {
                    terminal_error = Some(error);
                    break;
                }
            }
        }

        server.finish(1);
        assert_eq!(received, artifact);
        let error = terminal_error.expect("abrupt TLS shutdown must surface as a body error");
        let error_chain = format!("{error:?}");
        assert!(
            error_chain.contains("close_notify"),
            "unexpected body error: {error_chain}"
        );
    }

    #[tokio::test]
    async fn https_download_retries_truncation_then_accepts_exact_identity_without_close_notify() {
        let root = tempfile::tempdir().unwrap();
        let artifact = b"complete artifact delivered through the gateway".to_vec();
        let truncated = artifact[..artifact.len() - 7].to_vec();
        let server = AbruptHttpsServer::spawn(vec![truncated, artifact.clone()]);
        let context = tls_context(root.path(), &server);
        std::fs::write(&context.credential_file, "deployment-token").unwrap();
        let client = context.client().unwrap();
        let target = root.path().join("artifact.bin");

        context
            .download_to(
                &client,
                "storage_get",
                "/objects/artifact",
                &format!("sha256:{:x}", Sha256::digest(&artifact)),
                artifact.len() as u64,
                &target,
            )
            .await
            .expect("exact size and digest make a missing close_notify non-ambiguous");

        server.finish(2);
        assert_eq!(fs::read(&target).await.unwrap(), artifact);
        assert_no_download_temporaries(root.path());
    }

    #[tokio::test]
    async fn https_download_never_masks_wrong_digest_behind_missing_close_notify() {
        let root = tempfile::tempdir().unwrap();
        let expected = b"expected-content".to_vec();
        let altered = b"tampered-content".to_vec();
        assert_eq!(expected.len(), altered.len());
        let server = AbruptHttpsServer::spawn(vec![altered]);
        let context = tls_context(root.path(), &server);
        std::fs::write(&context.credential_file, "deployment-token").unwrap();
        let client = context.client().unwrap();
        let target = root.path().join("artifact.bin");

        let error = context
            .download_to(
                &client,
                "storage_get",
                "/objects/artifact",
                &format!("sha256:{:x}", Sha256::digest(&expected)),
                expected.len() as u64,
                &target,
            )
            .await
            .expect_err("matching size cannot substitute for the expected digest");

        server.finish(1);
        assert!(error.to_string().contains("SHA-256"));
        assert!(!target.exists());
        assert_no_download_temporaries(root.path());
    }

    #[tokio::test]
    async fn https_download_exhausts_bounded_retries_for_incomplete_streams() {
        let root = tempfile::tempdir().unwrap();
        let expected = b"artifact-that-must-not-be-partially-published".to_vec();
        let partial = expected[..8].to_vec();
        let server = AbruptHttpsServer::spawn(vec![partial.clone(), partial.clone(), partial]);
        let context = tls_context(root.path(), &server);
        std::fs::write(&context.credential_file, "deployment-token").unwrap();
        let client = context.client().unwrap();
        let target = root.path().join("artifact.bin");

        let error = context
            .download_to(
                &client,
                "storage_get",
                "/objects/artifact",
                &format!("sha256:{:x}", Sha256::digest(&expected)),
                expected.len() as u64,
                &target,
            )
            .await
            .expect_err("three incomplete HTTPS responses must fail closed");

        server.finish(DOWNLOAD_MAX_ATTEMPTS);
        assert!(error.to_string().contains("before declared size"));
        assert!(!target.exists());
        assert_no_download_temporaries(root.path());
    }

    fn tls_context(root: &Path, server: &AbruptHttpsServer) -> ServiceContext {
        let ca_file = root.join("gateway-ca.pem");
        std::fs::write(&ca_file, &server.ca_pem).unwrap();
        let mut context = context(root);
        context.gateway.origin = server.origin.clone();
        context.gateway.ca_file = Some(ca_file);
        context
    }

    fn assert_no_download_temporaries(root: &Path) {
        let leftovers = std::fs::read_dir(root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ojos-download-") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "temporary downloads leaked: {leftovers:?}"
        );
    }

    struct ScriptedHttpServer {
        origin: String,
        accepted: Arc<AtomicUsize>,
        handle: JoinHandle<()>,
    }

    impl ScriptedHttpServer {
        fn spawn<F>(request_count: usize, mut respond: F) -> Self
        where
            F: FnMut(usize, &[u8], &mut std::net::TcpStream) + Send + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted HTTP server");
            listener
                .set_nonblocking(true)
                .expect("set scripted HTTP listener nonblocking");
            let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
            let accepted = Arc::new(AtomicUsize::new(0));
            let server_accepted = accepted.clone();
            let handle = std::thread::spawn(move || {
                for request_index in 0..request_count {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let (mut stream, _) = loop {
                        match listener.accept() {
                            Ok(connection) => break connection,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    Instant::now() < deadline,
                                    "timed out waiting for scripted HTTP request"
                                );
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(error) => panic!("accept scripted HTTP request: {error}"),
                        }
                    };
                    stream
                        .set_nonblocking(false)
                        .expect("set scripted HTTP stream blocking");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let headers = read_request_headers(&mut stream);
                    respond(request_index, &headers, &mut stream);
                    stream.flush().unwrap();
                    server_accepted.fetch_add(1, Ordering::SeqCst);
                }
            });
            Self {
                origin,
                accepted,
                handle,
            }
        }

        fn finish(self, expected_requests: usize) {
            self.handle.join().expect("join scripted HTTP server");
            assert_eq!(self.accepted.load(Ordering::SeqCst), expected_requests);
        }
    }

    fn bearer_token_from_request(headers: &[u8]) -> Option<&str> {
        std::str::from_utf8(headers).ok()?.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().strip_prefix("Bearer "))
                .flatten()
        })
    }

    struct AbruptHttpsServer {
        origin: String,
        ca_pem: String,
        accepted: Arc<AtomicUsize>,
        handle: JoinHandle<()>,
    }

    impl AbruptHttpsServer {
        fn spawn(responses: Vec<Vec<u8>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind abrupt HTTPS server");
            listener
                .set_nonblocking(true)
                .expect("set abrupt HTTPS listener nonblocking");
            let origin = format!(
                "https://localhost:{}",
                listener.local_addr().unwrap().port()
            );
            let (ca_pem, server_config) = test_tls_material();
            let accepted = Arc::new(AtomicUsize::new(0));
            let server_accepted = accepted.clone();
            let handle = std::thread::spawn(move || {
                for body in responses {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let (tcp, _) = loop {
                        match listener.accept() {
                            Ok(connection) => break connection,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(
                                    Instant::now() < deadline,
                                    "timed out waiting for abrupt HTTPS request"
                                );
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(error) => panic!("accept abrupt HTTPS request: {error}"),
                        }
                    };
                    tcp.set_nonblocking(false).unwrap();
                    tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    let connection = rustls::ServerConnection::new(server_config.clone()).unwrap();
                    let mut stream = rustls::StreamOwned::new(connection, tcp);
                    let _ = read_request_headers(&mut stream);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                    stream.flush().unwrap();
                    server_accepted.fetch_add(1, Ordering::SeqCst);

                    // Intentionally omit both the terminating zero-sized HTTP
                    // chunk and TLS close_notify. This reproduces the Gateway
                    // EOF observed by the real Worker without weakening the
                    // client's rustls configuration.
                    let (_tls, tcp) = stream.into_parts();
                    let _ = tcp.shutdown(Shutdown::Both);
                }
            });
            Self {
                origin,
                ca_pem,
                accepted,
                handle,
            }
        }

        fn finish(self, expected_requests: usize) {
            self.handle.join().expect("join abrupt HTTPS server");
            assert_eq!(self.accepted.load(Ordering::SeqCst), expected_requests);
        }
    }

    fn read_request_headers(stream: &mut impl Read) -> Vec<u8> {
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).expect("read HTTPS request");
            assert!(count > 0, "client closed before sending request headers");
            received.extend_from_slice(&chunk[..count]);
            assert!(received.len() <= 32 * 1024, "request headers are too large");
        }
        received
    }

    fn test_tls_material() -> (String, Arc<rustls::ServerConfig>) {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let ca_key = KeyPair::generate().unwrap();
        let ca_certificate = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::from_params(&ca_params, &ca_key);

        let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let server_key = KeyPair::generate().unwrap();
        let server_certificate = server_params.signed_by(&server_key, &issuer).unwrap();
        let private_key =
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server_key.serialize_der()));
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![server_certificate.der().clone()], private_key)
        .unwrap();
        (ca_certificate.pem(), Arc::new(server_config))
    }
}
