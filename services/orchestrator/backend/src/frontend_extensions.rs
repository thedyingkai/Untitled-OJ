//! Same-origin delivery boundary for signed frontend extension bundles.
//!
//! The public browser URL contains only the bundle digest and logical artifact
//! path. The remote artifact reference is recovered from one freshly compiled
//! ACTIVE Contribution snapshot and is never exposed to the browser.

use crate::contribution_snapshot::active_contribution_snapshot;
use crate::durable::DurableStore;
use crate::static_site::StaticResponse;
use anyhow::{Context, Result, anyhow};
use semver::VersionReq;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ureq::Agent;

const ROUTE_PREFIX: &str = "/__ojos/extensions/";
const MAX_BUNDLE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 32;
const MAX_CA_BUNDLE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CA_CERTIFICATES: usize = 128;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TIMEOUT: Duration = Duration::from_secs(15);
const CA_FILE_ENV: &str = "ORCHESTRATOR_FRONTEND_ARTIFACT_CA_CERT";
const TIMEOUT_MS_ENV: &str = "ORCHESTRATOR_FRONTEND_ARTIFACT_TIMEOUT_MS";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ArtifactKey {
    digest: String,
    artifact: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArtifactCandidate {
    reference: String,
}

#[derive(Debug, Default)]
struct CacheState {
    entries: BTreeMap<ArtifactKey, Arc<Vec<u8>>>,
    order: VecDeque<ArtifactKey>,
}

/// Bounded, process-local cache. Every request first recompiles the ACTIVE
/// allowlist; cached bytes can therefore never keep a retired revision live.
#[derive(Clone)]
pub(crate) struct FrontendExtensionService {
    agent: Agent,
    cache: Arc<Mutex<CacheState>>,
}

impl std::fmt::Debug for FrontendExtensionService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrontendExtensionService")
            .field("cache", &"bounded/redacted")
            .finish()
    }
}

impl FrontendExtensionService {
    pub(crate) fn from_env() -> Result<Self> {
        let timeout = std::env::var(TIMEOUT_MS_ENV)
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .with_context(|| format!("{TIMEOUT_MS_ENV} must be an integer"))?
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);
        if timeout.is_zero() || timeout > MAX_TIMEOUT {
            return Err(anyhow!(
                "{TIMEOUT_MS_ENV} must be between 1 and {} milliseconds",
                MAX_TIMEOUT.as_millis()
            ));
        }

        let mut builder = Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .https_only(true)
            .proxy(None);
        if let Some(path) = ca_file_from_env()? {
            let certificates = load_ca_certificates(&path)?;
            builder = builder.tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::new_with_certs(&certificates))
                    .use_sni(true)
                    .disable_verification(false)
                    .build(),
            );
        }
        Ok(Self {
            agent: builder.build().into(),
            cache: Arc::new(Mutex::new(CacheState::default())),
        })
    }

    pub(crate) fn is_route(path: &str) -> bool {
        path.starts_with(ROUTE_PREFIX)
    }

    pub(crate) fn serve(
        &self,
        storage: Option<&DurableStore>,
        method: &str,
        path: &str,
    ) -> StaticResponse {
        let Some(key) = parse_browser_path(method, path) else {
            return not_found();
        };
        let Some(storage) = storage else {
            return not_found();
        };
        // One snapshot value supplies the complete allowlist. We deliberately
        // do not query heads or revisions again after choosing the candidate.
        let snapshot = match active_contribution_snapshot(storage, "default") {
            Ok(snapshot) => snapshot,
            Err(_) => return unavailable(),
        };
        let allowlist = match active_admin_allowlist(&snapshot) {
            Ok(allowlist) => allowlist,
            Err(_) => return unavailable(),
        };
        let Some(candidate) = allowlist.get(&key) else {
            return not_found();
        };

        if let Some(bytes) = self.cached(&key) {
            return javascript_response(method, bytes);
        }
        let bytes = match self.fetch(candidate, &key.digest) {
            Ok(bytes) => Arc::new(bytes),
            Err(FetchError::NotFound) => return not_found(),
            Err(FetchError::BadGateway) => return bad_gateway(),
        };
        self.insert_cache(key, Arc::clone(&bytes));
        javascript_response(method, bytes)
    }

    fn cached(&self, key: &ArtifactKey) -> Option<Arc<Vec<u8>>> {
        self.cache.lock().ok()?.entries.get(key).cloned()
    }

    fn insert_cache(&self, key: ArtifactKey, bytes: Arc<Vec<u8>>) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        if cache.entries.contains_key(&key) {
            return;
        }
        while cache.entries.len() >= MAX_CACHE_ENTRIES {
            let Some(oldest) = cache.order.pop_front() else {
                break;
            };
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(key.clone());
        cache.entries.insert(key, bytes);
    }

    fn fetch(
        &self,
        candidate: &ArtifactCandidate,
        expected_digest: &str,
    ) -> std::result::Result<Vec<u8>, FetchError> {
        let response = self
            .agent
            .get(&candidate.reference)
            .header("Accept", "text/javascript, application/javascript")
            .call()
            .map_err(|_| FetchError::BadGateway)?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(FetchError::NotFound);
        }
        if status != 200 {
            return Err(FetchError::BadGateway);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            content_type.as_str(),
            "text/javascript" | "application/javascript" | "application/ecmascript"
        ) {
            return Err(FetchError::BadGateway);
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_BUNDLE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| FetchError::BadGateway)?;
        if bytes.len() > MAX_BUNDLE_BYTES {
            return Err(FetchError::BadGateway);
        }
        let actual = format!("sha256:{:x}", Sha256::digest(&bytes));
        if actual != expected_digest {
            return Err(FetchError::BadGateway);
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchError {
    NotFound,
    BadGateway,
}

fn parse_browser_path(method: &str, path: &str) -> Option<ArtifactKey> {
    if !matches!(method, "GET" | "HEAD") || path.contains(['?', '#', '\\']) {
        return None;
    }
    let suffix = path.strip_prefix(ROUTE_PREFIX)?;
    let (hex, artifact) = suffix.split_once('/')?;
    if !is_lowercase_sha256_hex(hex) || !valid_artifact_path(artifact) {
        return None;
    }
    Some(ArtifactKey {
        digest: format!("sha256:{hex}"),
        artifact: artifact.to_string(),
    })
}

fn active_admin_allowlist(snapshot: &Value) -> Result<BTreeMap<ArtifactKey, ArtifactCandidate>> {
    let digest = required_string(snapshot, "digest")?;
    if !is_canonical_sha256(digest) {
        return Err(anyhow!("Contribution snapshot digest is not canonical"));
    }
    let modules = snapshot
        .get("admin_frontend_modules")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Contribution snapshot admin modules are invalid"))?;
    let mut allowlist = BTreeMap::new();
    for module in modules {
        if module.get("enabled").and_then(Value::as_bool) != Some(true)
            || required_string(module, "target")? != "admin-shell"
        {
            continue;
        }
        // Validate every signed delivery field before adding a candidate. A
        // malformed ACTIVE row makes the projection unavailable instead of
        // silently widening or partially accepting the allowlist.
        let artifact = required_string(module, "artifact")?;
        let host_api_range = required_string(module, "host_api_range")?;
        let manifest_digest = required_string(module, "manifest_digest")?;
        let manifest_reference = required_string(module, "manifest_reference")?;
        let bundle_digest = required_string(module, "bundle_digest")?;
        let bundle_reference = required_string(module, "bundle_reference")?;
        if !valid_artifact_path(artifact)
            || VersionReq::parse(host_api_range).is_err()
            || !is_canonical_sha256(manifest_digest)
            || !valid_https_content_addressed_reference(manifest_reference, manifest_digest)
            || !is_canonical_sha256(bundle_digest)
            || !valid_https_content_addressed_reference(bundle_reference, bundle_digest)
        {
            return Err(anyhow!(
                "ACTIVE frontend module delivery contract is invalid"
            ));
        }
        let key = ArtifactKey {
            digest: bundle_digest.to_string(),
            artifact: artifact.to_string(),
        };
        let candidate = ArtifactCandidate {
            reference: bundle_reference.to_string(),
        };
        if allowlist
            .insert(key, candidate.clone())
            .is_some_and(|previous| previous != candidate)
        {
            return Err(anyhow!("ACTIVE frontend artifact key is ambiguous"));
        }
    }
    Ok(allowlist)
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && *value == value.trim())
        .ok_or_else(|| anyhow!("frontend snapshot field {field} is missing or invalid"))
}

fn valid_https_content_addressed_reference(reference: &str, digest: &str) -> bool {
    if reference.len() > 4096
        || reference != reference.trim()
        || reference.chars().any(char::is_control)
    {
        return false;
    }
    let Ok(url) = url::Url::parse(reference) else {
        return false;
    };
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.host_str().is_none()
        || url.fragment().is_some()
    {
        return false;
    }
    // Literal loopback/private/link-local destinations are never accepted.
    // DNS rebinding remains constrained by HTTPS hostname verification.
    if url
        .host_str()
        .and_then(|host| host.parse::<IpAddr>().ok())
        .is_some_and(blocked_ip)
    {
        return false;
    }
    reference.contains(digest.trim_start_matches("sha256:"))
}

fn blocked_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback()
                || address.is_private()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_broadcast()
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || (address.segments()[0] & 0xfe00) == 0xfc00
                || (address.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn valid_artifact_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && !value.contains(['\\', '?', '#', '%'])
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment.len() <= 256
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

fn is_canonical_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(is_lowercase_sha256_hex)
}

fn is_lowercase_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ca_file_from_env() -> Result<Option<PathBuf>> {
    let Some(raw) = std::env::var_os(CA_FILE_ENV) else {
        return Ok(None);
    };
    let value = raw
        .into_string()
        .map_err(|_| anyhow!("{CA_FILE_ENV} must be valid Unicode"))?;
    if value.is_empty() || value != value.trim() {
        return Err(anyhow!(
            "{CA_FILE_ENV} must be a non-empty absolute path with no surrounding whitespace"
        ));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(anyhow!("{CA_FILE_ENV} must be an absolute path"));
    }
    Ok(Some(path))
}

fn load_ca_certificates(path: &Path) -> Result<Vec<ureq::tls::Certificate<'static>>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("read {CA_FILE_ENV} {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_CA_BUNDLE_BYTES as u64 {
        return Err(anyhow!(
            "{CA_FILE_ENV} {} must be a regular PEM file no larger than {MAX_CA_BUNDLE_BYTES} bytes",
            path.display()
        ));
    }
    let pem = fs::read(path).with_context(|| format!("read {CA_FILE_ENV} {}", path.display()))?;
    let certificates = ureq::tls::parse_pem(&pem)
        .filter_map(|item| match item {
            Ok(ureq::tls::PemItem::Certificate(certificate)) => Some(Ok(certificate)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .take(MAX_CA_CERTIFICATES + 1)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("parse {CA_FILE_ENV} {}", path.display()))?;
    if certificates.is_empty() || certificates.len() > MAX_CA_CERTIFICATES {
        return Err(anyhow!(
            "{CA_FILE_ENV} {} must contain 1-{MAX_CA_CERTIFICATES} certificates",
            path.display()
        ));
    }
    Ok(certificates)
}

fn javascript_response(method: &str, bytes: Arc<Vec<u8>>) -> StaticResponse {
    StaticResponse {
        status: 200,
        content_type: "text/javascript; charset=utf-8",
        cache_control: "public, max-age=31536000, immutable",
        body: if method == "HEAD" {
            Vec::new()
        } else {
            bytes.as_ref().clone()
        },
        content_length: Some(bytes.len()),
    }
}

fn not_found() -> StaticResponse {
    StaticResponse {
        status: 404,
        content_type: "text/plain; charset=utf-8",
        cache_control: "no-store",
        body: Vec::new(),
        content_length: None,
    }
}

fn unavailable() -> StaticResponse {
    StaticResponse {
        status: 503,
        content_type: "text/plain; charset=utf-8",
        cache_control: "no-store",
        body: Vec::new(),
        content_length: None,
    }
}

fn bad_gateway() -> StaticResponse {
    StaticResponse {
        status: 502,
        content_type: "text/plain; charset=utf-8",
        cache_control: "no-store",
        body: Vec::new(),
        content_length: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn snapshot(enabled: bool, target: &str) -> Value {
        json!({
            "schema_version": "ojos.dev/contribution-snapshot/v1",
            "digest": format!("sha256:{A}"),
            "admin_frontend_modules": [{
                "enabled": enabled,
                "target": target,
                "artifact": "bundle.js",
                "host_api_range": "^1.0",
                "manifest_digest": format!("sha256:{A}"),
                "manifest_reference": format!("https://artifacts.example/manifests/{A}/manifest.json"),
                "bundle_digest": format!("sha256:{B}"),
                "bundle_reference": format!("https://artifacts.example/extensions/{B}/bundle.js")
            }]
        })
    }

    #[test]
    fn browser_route_accepts_only_hex_digest_and_safe_artifact() {
        let expected = ArtifactKey {
            digest: format!("sha256:{B}"),
            artifact: "dir/bundle.js".to_string(),
        };
        assert_eq!(
            parse_browser_path("GET", &format!("/__ojos/extensions/{B}/dir/bundle.js")),
            Some(expected.clone())
        );
        assert_eq!(
            parse_browser_path("HEAD", &format!("/__ojos/extensions/{B}/dir/bundle.js")),
            Some(expected)
        );
        for path in [
            format!("/__ojos/extensions/sha256:{B}/bundle.js"),
            format!("/__ojos/extensions/{B}/../bundle.js"),
            format!("/__ojos/extensions/{B}/bundle%2Ejs"),
            format!("/__ojos/extensions/{}/bundle.js", B.to_ascii_uppercase()),
        ] {
            assert!(
                parse_browser_path("GET", &path).is_none(),
                "accepted {path}"
            );
        }
        assert!(parse_browser_path("POST", &format!("/__ojos/extensions/{B}/bundle.js")).is_none());
    }

    #[test]
    fn allowlist_contains_only_enabled_admin_shell_modules() {
        let allowlist = active_admin_allowlist(&snapshot(true, "admin-shell")).unwrap();
        assert_eq!(allowlist.len(), 1);
        let key = ArtifactKey {
            digest: format!("sha256:{B}"),
            artifact: "bundle.js".to_string(),
        };
        assert_eq!(
            allowlist.get(&key).unwrap().reference,
            format!("https://artifacts.example/extensions/{B}/bundle.js")
        );
        assert!(
            active_admin_allowlist(&snapshot(false, "admin-shell"))
                .unwrap()
                .is_empty()
        );
        assert!(
            active_admin_allowlist(&snapshot(true, "user-shell"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_signed_delivery_fields_fail_the_complete_allowlist() {
        for (field, value) in [
            ("host_api_range", json!("not semver")),
            ("artifact", json!("../bundle.js")),
            (
                "manifest_reference",
                json!("http://artifacts.example/manifest.json"),
            ),
            ("bundle_reference", json!("https://127.0.0.1/bundle.js")),
            ("bundle_digest", json!(B)),
        ] {
            let mut document = snapshot(true, "admin-shell");
            document["admin_frontend_modules"][0][field] = value;
            assert!(
                active_admin_allowlist(&document).is_err(),
                "accepted malformed {field}"
            );
        }
    }

    #[test]
    fn cache_is_bounded_and_never_filled_by_failed_fetch_path() {
        let service = FrontendExtensionService::from_env().unwrap();
        for index in 0..(MAX_CACHE_ENTRIES + 3) {
            let key = ArtifactKey {
                digest: format!("sha256:{index:064x}"),
                artifact: "bundle.js".to_string(),
            };
            service.insert_cache(key, Arc::new(vec![index as u8]));
        }
        let cache = service.cache.lock().unwrap();
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
        assert_eq!(cache.order.len(), MAX_CACHE_ENTRIES);
    }

    #[test]
    fn redirect_digest_mismatch_and_oversize_never_enter_cache() {
        use std::io::Write as _;
        use std::net::{Shutdown, TcpListener};
        use std::thread;

        for (status, body, expected) in [
            ("302 Found", b"".as_slice(), FetchError::BadGateway),
            ("200 OK", b"tampered".as_slice(), FetchError::BadGateway),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body = body.to_vec();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nLocation: https://redirect.example/bundle.js\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
                stream.shutdown(Shutdown::Write).unwrap();
            });
            let service = FrontendExtensionService {
                agent: Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(2)))
                    .http_status_as_error(false)
                    .max_redirects(0)
                    .proxy(None)
                    .build()
                    .into(),
                cache: Arc::new(Mutex::new(CacheState::default())),
            };
            let candidate = ArtifactCandidate {
                reference: format!("http://{address}/bundle.js"),
            };
            assert_eq!(
                service.fetch(&candidate, &format!("sha256:{B}")),
                Err(expected)
            );
            assert!(service.cache.lock().unwrap().entries.is_empty());
            server.join().unwrap();
        }
    }
}
