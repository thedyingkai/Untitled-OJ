//! Remote control-plane OIDC bearer verification.
//!
//! The verifier is deliberately synchronous because the daemon's HTTP boundary is
//! synchronous. Discovery and the first JWKS load happen before a production
//! listener starts; later network access is limited to bounded JWKS refreshes.

use crate::auth::{OidcPrincipalVerifier, Principal, PrincipalSource, PrincipalVerificationError};
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use orchestrator_legacy::V1Role;
use serde::Deserialize;
use serde_json::Value;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use ureq::Agent;
use ureq::http::Uri;
use ureq::tls::{Certificate, RootCerts, TlsConfig, TlsProvider};
use x509_parser::parse_x509_certificate;

const DEFAULT_ROLE_CLAIM: &str = "roles";
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OIDC_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_JWKS_KEYS: usize = 128;
const MAX_OIDC_CA_BUNDLE_BYTES: usize = 4 * 1024 * 1024;
const MAX_OIDC_CA_CERTIFICATES: usize = 128;

#[derive(Debug, Error)]
pub(crate) enum OidcConfigurationError {
    #[error("{0}")]
    Invalid(String),
    #[error("OIDC discovery failed: {0}")]
    Discovery(String),
}

#[derive(Clone)]
struct OidcCaBundle {
    source: PathBuf,
    certificates: Vec<Certificate<'static>>,
}

impl std::fmt::Debug for OidcCaBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OidcCaBundle")
            .field("source", &self.source)
            .field("certificate_count", &self.certificates.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OidcConfig {
    issuer: String,
    audience: String,
    role_claim: String,
    viewer_role: String,
    operator_role: String,
    admin_role: String,
    cache_ttl: Duration,
    http_timeout: Duration,
    allow_insecure_loopback: bool,
    ca_bundle: Option<OidcCaBundle>,
}

impl OidcConfig {
    pub(crate) fn from_env() -> Result<Self, OidcConfigurationError> {
        let issuer = required_env("ORCHESTRATOR_OIDC_ISSUER")?;
        let audience = required_env("ORCHESTRATOR_OIDC_AUDIENCE")?;
        let role_claim = optional_env("ORCHESTRATOR_OIDC_ROLE_CLAIM")
            .unwrap_or_else(|| DEFAULT_ROLE_CLAIM.to_string());
        let viewer_role =
            optional_env("ORCHESTRATOR_OIDC_VIEWER_ROLE").unwrap_or_else(|| "viewer".to_string());
        let operator_role = optional_env("ORCHESTRATOR_OIDC_OPERATOR_ROLE")
            .unwrap_or_else(|| "operator".to_string());
        let admin_role =
            optional_env("ORCHESTRATOR_OIDC_ADMIN_ROLE").unwrap_or_else(|| "admin".to_string());
        let cache_ttl = duration_env(
            "ORCHESTRATOR_OIDC_JWKS_CACHE_SECONDS",
            DEFAULT_CACHE_TTL,
            30,
            3600,
        )?;
        let http_timeout = duration_env(
            "ORCHESTRATOR_OIDC_HTTP_TIMEOUT_SECONDS",
            DEFAULT_HTTP_TIMEOUT,
            1,
            30,
        )?;
        let ca_bundle = oidc_ca_bundle_from_env()?;
        Self::new(
            issuer,
            audience,
            role_claim,
            [viewer_role, operator_role, admin_role],
            cache_ttl,
            http_timeout,
            false,
            ca_bundle,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        issuer: String,
        audience: String,
        role_claim: String,
        role_values: [String; 3],
        cache_ttl: Duration,
        http_timeout: Duration,
        allow_insecure_loopback: bool,
        ca_bundle: Option<OidcCaBundle>,
    ) -> Result<Self, OidcConfigurationError> {
        validate_url(&issuer, allow_insecure_loopback, "OIDC issuer")?;
        validate_claim_name(&role_claim)?;
        if audience.trim() != audience || audience.is_empty() {
            return Err(OidcConfigurationError::Invalid(
                "ORCHESTRATOR_OIDC_AUDIENCE must be non-empty and contain no surrounding whitespace"
                    .to_string(),
            ));
        }
        if role_values.iter().any(|value| value.is_empty()) {
            return Err(OidcConfigurationError::Invalid(
                "OIDC role mapping values must be non-empty".to_string(),
            ));
        }
        if role_values[0] == role_values[1]
            || role_values[0] == role_values[2]
            || role_values[1] == role_values[2]
        {
            return Err(OidcConfigurationError::Invalid(
                "OIDC viewer/operator/admin role mapping values must be distinct".to_string(),
            ));
        }
        Ok(Self {
            issuer,
            audience,
            role_claim,
            viewer_role: role_values[0].clone(),
            operator_role: role_values[1].clone(),
            admin_role: role_values[2].clone(),
            cache_ttl,
            http_timeout,
            allow_insecure_loopback,
            ca_bundle,
        })
    }
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
    #[serde(default)]
    authorization_endpoint: Option<String>,
    #[serde(default)]
    token_endpoint: Option<String>,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
}

#[derive(Debug)]
struct JwksCache {
    set: JwkSet,
    loaded_at: Instant,
}

/// A strict OIDC access-token verifier for the remote control plane.
pub(crate) struct OidcVerifier {
    config: OidcConfig,
    jwks_uri: String,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    agent: Agent,
    cache: Mutex<JwksCache>,
}

#[derive(Debug)]
pub(crate) struct OidcCodeIdentity {
    pub(crate) principal: Principal,
    pub(crate) expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    id_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl OidcVerifier {
    /// Performs discovery and preloads JWKS. This is called before the
    /// PostgreSQL-backed production listener starts, so bad identity
    /// configuration cannot become a partially-ready daemon.
    pub(crate) fn discover(config: OidcConfig) -> Result<Self, OidcConfigurationError> {
        let mut agent_config = Agent::config_builder()
            .timeout_global(Some(config.http_timeout))
            .http_status_as_error(false)
            .max_redirects(0);
        if let Some(ca_bundle) = &config.ca_bundle {
            // Supplying a private issuer CA is an explicit trust decision. Use
            // exactly that bundle as the trust store while keeping rustls' normal
            // certificate-chain, validity, hostname and SNI verification enabled.
            let tls = TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::new_with_certs(&ca_bundle.certificates))
                .use_sni(true)
                .disable_verification(false)
                .build();
            agent_config = agent_config.tls_config(tls);
        }
        let agent: Agent = agent_config.build().into();
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            config.issuer.trim_end_matches('/')
        );
        let discovery: DiscoveryDocument =
            fetch_json(&agent, &discovery_url, MAX_OIDC_DOCUMENT_BYTES)
                .map_err(OidcConfigurationError::Discovery)?;
        if discovery.issuer != config.issuer {
            return Err(OidcConfigurationError::Invalid(format!(
                "OIDC discovery issuer mismatch: expected {}, received {}",
                config.issuer, discovery.issuer
            )));
        }
        validate_url(
            &discovery.jwks_uri,
            config.allow_insecure_loopback,
            "OIDC jwks_uri",
        )?;
        for (endpoint, label) in [
            (
                discovery.authorization_endpoint.as_deref(),
                "OIDC authorization_endpoint",
            ),
            (discovery.token_endpoint.as_deref(), "OIDC token_endpoint"),
        ] {
            if let Some(endpoint) = endpoint {
                validate_url(endpoint, config.allow_insecure_loopback, label)?;
            }
        }
        if !discovery
            .id_token_signing_alg_values_supported
            .iter()
            .any(|algorithm| algorithm == "RS256")
        {
            return Err(OidcConfigurationError::Invalid(
                "OIDC provider does not advertise the required RS256 signing algorithm".to_string(),
            ));
        }
        let set =
            load_jwks(&agent, &discovery.jwks_uri).map_err(OidcConfigurationError::Discovery)?;
        Ok(Self {
            config,
            jwks_uri: discovery.jwks_uri,
            authorization_endpoint: discovery.authorization_endpoint,
            token_endpoint: discovery.token_endpoint,
            agent,
            cache: Mutex::new(JwksCache {
                set,
                loaded_at: Instant::now(),
            }),
        })
    }

    fn decode_claims(
        &self,
        token: &str,
        kid: &str,
        force_refresh: bool,
    ) -> Result<Value, VerifyAttemptError> {
        let key = self.key_for(kid, force_refresh)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.config.audience.as_str()]);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.leeway = 0;
        decode::<Value>(token, &key, &validation)
            .map(|data| data.claims)
            .map_err(VerifyAttemptError::Jwt)
    }

    fn decode_claims_for_audience(
        &self,
        token: &str,
        audience: &str,
    ) -> Result<Value, PrincipalVerificationError> {
        let header = decode_header(token)
            .map_err(|_| PrincipalVerificationError::new("OIDC ID token header is invalid"))?;
        if header.alg != Algorithm::RS256 {
            return Err(PrincipalVerificationError::new(
                "OIDC ID token signing algorithm is not allowed",
            ));
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PrincipalVerificationError::new("OIDC ID token is missing kid"))?;
        let decode_once = |force_refresh| {
            let key = self.key_for(kid, force_refresh)?;
            let mut validation = Validation::new(Algorithm::RS256);
            validation.set_audience(&[audience]);
            validation.set_issuer(&[self.config.issuer.as_str()]);
            validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
            validation.validate_exp = true;
            validation.validate_nbf = true;
            validation.leeway = 0;
            decode::<Value>(token, &key, &validation)
                .map(|data| data.claims)
                .map_err(VerifyAttemptError::Jwt)
        };
        match decode_once(false) {
            Ok(claims) => Ok(claims),
            Err(error) if error.should_refresh() => {
                decode_once(true).map_err(PrincipalVerificationError::from)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn issuer(&self) -> &str {
        &self.config.issuer
    }

    pub(crate) fn audience(&self) -> &str {
        &self.config.audience
    }

    pub(crate) fn authorization_endpoint(&self) -> Option<&str> {
        self.authorization_endpoint.as_deref()
    }

    pub(crate) fn token_endpoint(&self) -> Option<&str> {
        self.token_endpoint.as_deref()
    }

    pub(crate) fn exchange_authorization_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
        client_id: &str,
        expected_nonce: &str,
    ) -> Result<OidcCodeIdentity, PrincipalVerificationError> {
        if code.trim().is_empty() || code_verifier.trim().is_empty() {
            return Err(PrincipalVerificationError::new(
                "OIDC authorization code or PKCE verifier is missing",
            ));
        }
        let endpoint = self.token_endpoint().ok_or_else(|| {
            PrincipalVerificationError::new("OIDC discovery does not advertise token_endpoint")
        })?;
        let form = form_encode(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ]);
        let response = self
            .agent
            .post(endpoint)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(form)
            .map_err(|error| {
                PrincipalVerificationError::new(format!("OIDC token exchange failed: {error}"))
            })?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_OIDC_DOCUMENT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                PrincipalVerificationError::new(format!("read OIDC token response failed: {error}"))
            })?;
        if status != 200 {
            return Err(PrincipalVerificationError::new(format!(
                "OIDC token endpoint returned HTTP {status}"
            )));
        }
        if bytes.len() > MAX_OIDC_DOCUMENT_BYTES {
            return Err(PrincipalVerificationError::new(
                "OIDC token response exceeds 1 MiB",
            ));
        }
        let tokens: TokenResponse = serde_json::from_slice(&bytes).map_err(|error| {
            PrincipalVerificationError::new(format!("OIDC token response is invalid: {error}"))
        })?;
        if !tokens.token_type.eq_ignore_ascii_case("Bearer") {
            return Err(PrincipalVerificationError::new(
                "OIDC token_type must be Bearer",
            ));
        }
        let principal = self
            .verify_bearer(Some(&format!("Bearer {}", tokens.access_token)))?
            .ok_or_else(|| PrincipalVerificationError::new("OIDC access token is missing"))?;
        let access_header = decode_header(&tokens.access_token)
            .map_err(|_| PrincipalVerificationError::new("OIDC access token header is invalid"))?;
        let access_kid = access_header
            .kid
            .as_deref()
            .ok_or_else(|| PrincipalVerificationError::new("OIDC access token is missing kid"))?;
        let access_claims = self
            .decode_claims(&tokens.access_token, access_kid, false)
            .map_err(PrincipalVerificationError::from)?;
        let id_claims = self.decode_claims_for_audience(&tokens.id_token, client_id)?;
        let nonce = id_claims
            .get("nonce")
            .and_then(Value::as_str)
            .ok_or_else(|| PrincipalVerificationError::new("OIDC ID token is missing nonce"))?;
        if !constant_time_str_eq(nonce, expected_nonce) {
            return Err(PrincipalVerificationError::new(
                "OIDC ID token nonce does not match the authorization request",
            ));
        }
        let id_subject = id_claims
            .get("sub")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PrincipalVerificationError::new("OIDC ID token subject is invalid"))?;
        if id_subject != principal.id() {
            return Err(PrincipalVerificationError::new(
                "OIDC access token and ID token subjects do not match",
            ));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let signed_ttl = [
            access_claims.get("exp").and_then(Value::as_u64),
            id_claims.get("exp").and_then(Value::as_u64),
        ]
        .into_iter()
        .flatten()
        .map(|expiry| expiry.saturating_sub(now))
        .min();
        let expires_in = match (tokens.expires_in, signed_ttl) {
            (Some(advertised), Some(signed)) => Some(advertised.min(signed)),
            (Some(advertised), None) => Some(advertised),
            (None, Some(signed)) => Some(signed),
            (None, None) => None,
        };
        if expires_in == Some(0) {
            return Err(PrincipalVerificationError::new(
                "OIDC tokens have no remaining lifetime",
            ));
        }
        Ok(OidcCodeIdentity {
            principal,
            expires_in,
        })
    }

    fn key_for(&self, kid: &str, force_refresh: bool) -> Result<DecodingKey, VerifyAttemptError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| VerifyAttemptError::Configuration("JWKS cache lock poisoned".into()))?;
        if force_refresh || cache.loaded_at.elapsed() >= self.config.cache_ttl {
            cache.set = load_jwks(&self.agent, &self.jwks_uri)
                .map_err(VerifyAttemptError::Configuration)?;
            cache.loaded_at = Instant::now();
        }
        let matches = cache
            .set
            .keys
            .iter()
            .filter(|jwk| jwk.common.key_id.as_deref() == Some(kid))
            .collect::<Vec<_>>();
        let jwk = match matches.as_slice() {
            [] => return Err(VerifyAttemptError::MissingKey),
            [jwk] => *jwk,
            _ => {
                return Err(VerifyAttemptError::Configuration(format!(
                    "JWKS contains duplicate key id {kid}"
                )));
            }
        };
        validate_signing_key(jwk)?;
        DecodingKey::from_jwk(jwk).map_err(VerifyAttemptError::Jwt)
    }

    fn principal_from_claims(
        &self,
        claims: &Value,
    ) -> Result<Principal, PrincipalVerificationError> {
        let subject = claims
            .get("sub")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| PrincipalVerificationError::new("OIDC subject claim is invalid"))?;
        let role = self.map_role(claim_at_path(claims, &self.config.role_claim))?;
        Ok(Principal::verified(subject, role, PrincipalSource::Oidc))
    }

    fn map_role(&self, value: Option<&Value>) -> Result<V1Role, PrincipalVerificationError> {
        let values = match value {
            None | Some(Value::Null) => return Ok(V1Role::Viewer),
            Some(Value::String(value)) => vec![value.as_str()],
            Some(Value::Array(values)) => {
                if values.is_empty() {
                    return Ok(V1Role::Viewer);
                }
                values
                    .iter()
                    .map(|value| {
                        value.as_str().ok_or_else(|| {
                            PrincipalVerificationError::new(
                                "OIDC role claim array must contain only strings",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            Some(_) => {
                return Err(PrincipalVerificationError::new(
                    "OIDC role claim must be a string or string array",
                ));
            }
        };
        let mut effective = V1Role::Viewer;
        for value in values {
            let mapped = if value == self.config.viewer_role {
                V1Role::Viewer
            } else if value == self.config.operator_role {
                V1Role::Operator
            } else if value == self.config.admin_role {
                V1Role::Admin
            } else {
                return Err(PrincipalVerificationError::new(format!(
                    "OIDC role claim contains an unmapped value: {value}"
                )));
            };
            effective = effective.max(mapped);
        }
        Ok(effective)
    }
}

impl OidcPrincipalVerifier for OidcVerifier {
    fn verify_bearer(
        &self,
        authorization_header: Option<&str>,
    ) -> Result<Option<Principal>, PrincipalVerificationError> {
        let Some(header) = authorization_header else {
            return Ok(None);
        };
        let mut parts = header.split_ascii_whitespace();
        let scheme = parts.next();
        let token = parts.next();
        if !scheme.is_some_and(|value| value.eq_ignore_ascii_case("Bearer"))
            || token.is_none_or(str::is_empty)
            || parts.next().is_some()
        {
            return Err(PrincipalVerificationError::new(
                "Authorization must contain exactly one Bearer token",
            ));
        }
        let token = token.expect("checked above");
        let header = decode_header(token)
            .map_err(|_| PrincipalVerificationError::new("OIDC token header is invalid"))?;
        if header.alg != Algorithm::RS256 {
            return Err(PrincipalVerificationError::new(
                "OIDC token signing algorithm is not allowed",
            ));
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PrincipalVerificationError::new("OIDC token is missing kid"))?;

        let claims = match self.decode_claims(token, kid, false) {
            Ok(claims) => claims,
            Err(error) if error.should_refresh() => self
                .decode_claims(token, kid, true)
                .map_err(PrincipalVerificationError::from)?,
            Err(error) => return Err(error.into()),
        };
        self.principal_from_claims(&claims).map(Some)
    }
}

#[derive(Debug)]
enum VerifyAttemptError {
    MissingKey,
    Jwt(jsonwebtoken::errors::Error),
    Configuration(String),
}

impl VerifyAttemptError {
    fn should_refresh(&self) -> bool {
        match self {
            Self::MissingKey => true,
            Self::Jwt(error) => matches!(error.kind(), ErrorKind::InvalidSignature),
            Self::Configuration(_) => false,
        }
    }
}

impl From<VerifyAttemptError> for PrincipalVerificationError {
    fn from(error: VerifyAttemptError) -> Self {
        let detail = match error {
            VerifyAttemptError::MissingKey => "OIDC token kid is not present in JWKS".to_string(),
            VerifyAttemptError::Jwt(error) => format!("OIDC token validation failed: {error}"),
            VerifyAttemptError::Configuration(detail) => {
                format!("OIDC verification service is unavailable: {detail}")
            }
        };
        Self::new(detail)
    }
}

fn validate_signing_key(jwk: &Jwk) -> Result<(), VerifyAttemptError> {
    if matches!(jwk.common.public_key_use, Some(PublicKeyUse::Encryption)) {
        return Err(VerifyAttemptError::Configuration(
            "JWKS key is marked for encryption, not signature verification".to_string(),
        ));
    }
    if let Some(operations) = &jwk.common.key_operations
        && !operations.contains(&KeyOperations::Verify)
    {
        return Err(VerifyAttemptError::Configuration(
            "JWKS key does not permit verify operations".to_string(),
        ));
    }
    if jwk
        .common
        .key_algorithm
        .is_some_and(|algorithm| algorithm.to_string() != "RS256")
    {
        return Err(VerifyAttemptError::Configuration(
            "JWKS key algorithm does not match RS256".to_string(),
        ));
    }
    if !matches!(jwk.algorithm, AlgorithmParameters::RSA(_)) {
        return Err(VerifyAttemptError::Configuration(
            "JWKS key type is incompatible with RS256".to_string(),
        ));
    }
    Ok(())
}

fn load_jwks(agent: &Agent, uri: &str) -> Result<JwkSet, String> {
    let set: JwkSet = fetch_json(agent, uri, MAX_OIDC_DOCUMENT_BYTES)?;
    if set.keys.is_empty() {
        return Err("JWKS contains no keys".to_string());
    }
    if set.keys.len() > MAX_JWKS_KEYS {
        return Err(format!("JWKS exceeds the {MAX_JWKS_KEYS}-key limit"));
    }
    Ok(set)
}

fn fetch_json<T: for<'de> Deserialize<'de>>(
    agent: &Agent,
    url: &str,
    max_bytes: usize,
) -> Result<T, String> {
    let response = agent
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|error| format!("fetch {url} failed: {error}"))?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {url} failed: {error}"))?;
    if status != 200 {
        return Err(format!("fetch {url} returned HTTP {status}"));
    }
    if bytes.len() > max_bytes {
        return Err(format!("document from {url} exceeds {max_bytes} bytes"));
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {url} JSON failed: {error}"))
}

fn validate_url(
    value: &str,
    allow_insecure_loopback: bool,
    label: &str,
) -> Result<(), OidcConfigurationError> {
    if value.trim() != value || value.is_empty() {
        return Err(OidcConfigurationError::Invalid(format!(
            "{label} must be non-empty and contain no surrounding whitespace"
        )));
    }
    let uri = value.parse::<Uri>().map_err(|error| {
        OidcConfigurationError::Invalid(format!("{label} is not a valid URL: {error}"))
    })?;
    let authority = uri.authority().ok_or_else(|| {
        OidcConfigurationError::Invalid(format!("{label} must include an authority"))
    })?;
    if authority.as_str().contains('@') {
        return Err(OidcConfigurationError::Invalid(format!(
            "{label} must not include credentials"
        )));
    }
    let secure = uri.scheme_str() == Some("https");
    let loopback_test = allow_insecure_loopback
        && uri.scheme_str() == Some("http")
        && authority
            .host()
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !secure && !loopback_test {
        return Err(OidcConfigurationError::Invalid(format!(
            "{label} must use HTTPS"
        )));
    }
    Ok(())
}

fn validate_claim_name(value: &str) -> Result<(), OidcConfigurationError> {
    if value.is_empty()
        || value.split('.').any(str::is_empty)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(OidcConfigurationError::Invalid(
            "ORCHESTRATOR_OIDC_ROLE_CLAIM must be a simple claim name".to_string(),
        ));
    }
    Ok(())
}

fn claim_at_path<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(claims, |value, segment| value.get(segment))
}

fn form_encode(values: &[(&str, &str)]) -> String {
    values
        .iter()
        .map(|(name, value)| format!("{}={}", url_encode(name), url_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn constant_time_str_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn oidc_ca_bundle_from_env() -> Result<Option<OidcCaBundle>, OidcConfigurationError> {
    let value = match std::env::var("ORCHESTRATOR_OIDC_CA_CERT") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(OidcConfigurationError::Invalid(
                "ORCHESTRATOR_OIDC_CA_CERT must be valid Unicode".to_string(),
            ));
        }
    };
    if value.is_empty() || value.trim() != value {
        return Err(OidcConfigurationError::Invalid(
            "ORCHESTRATOR_OIDC_CA_CERT must be a non-empty absolute path with no surrounding whitespace"
                .to_string(),
        ));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(OidcConfigurationError::Invalid(
            "ORCHESTRATOR_OIDC_CA_CERT must be an absolute path".to_string(),
        ));
    }
    load_oidc_ca_bundle(&path).map(Some)
}

fn load_oidc_ca_bundle(path: &Path) -> Result<OidcCaBundle, OidcConfigurationError> {
    let file = File::open(path).map_err(|error| {
        OidcConfigurationError::Invalid(format!(
            "read ORCHESTRATOR_OIDC_CA_CERT {} failed: {error}",
            path.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        OidcConfigurationError::Invalid(format!(
            "inspect ORCHESTRATOR_OIDC_CA_CERT {} failed: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(OidcConfigurationError::Invalid(format!(
            "ORCHESTRATOR_OIDC_CA_CERT {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_OIDC_CA_BUNDLE_BYTES as u64 {
        return Err(OidcConfigurationError::Invalid(format!(
            "ORCHESTRATOR_OIDC_CA_CERT {} exceeds {MAX_OIDC_CA_BUNDLE_BYTES} bytes",
            path.display()
        )));
    }

    let mut bytes = Vec::new();
    file.take(MAX_OIDC_CA_BUNDLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            OidcConfigurationError::Invalid(format!(
                "read ORCHESTRATOR_OIDC_CA_CERT {} failed: {error}",
                path.display()
            ))
        })?;
    if bytes.len() > MAX_OIDC_CA_BUNDLE_BYTES {
        return Err(OidcConfigurationError::Invalid(format!(
            "ORCHESTRATOR_OIDC_CA_CERT {} exceeds {MAX_OIDC_CA_BUNDLE_BYTES} bytes",
            path.display()
        )));
    }

    let mut reader = BufReader::new(bytes.as_slice());
    let mut certificates = Vec::new();
    let mut roots = rustls::RootCertStore::empty();
    for (index, item) in rustls_pemfile::read_all(&mut reader).enumerate() {
        let item = item.map_err(|error| {
            OidcConfigurationError::Invalid(format!(
                "ORCHESTRATOR_OIDC_CA_CERT {} contains invalid PEM: {error}",
                path.display()
            ))
        })?;
        let rustls_pemfile::Item::X509Certificate(der) = item else {
            return Err(OidcConfigurationError::Invalid(format!(
                "ORCHESTRATOR_OIDC_CA_CERT {} contains a non-certificate PEM item",
                path.display()
            )));
        };
        if index >= MAX_OIDC_CA_CERTIFICATES {
            return Err(OidcConfigurationError::Invalid(format!(
                "ORCHESTRATOR_OIDC_CA_CERT {} contains more than {MAX_OIDC_CA_CERTIFICATES} certificates",
                path.display()
            )));
        }
        let (remaining, parsed) = parse_x509_certificate(der.as_ref()).map_err(|_| {
            OidcConfigurationError::Invalid(format!(
                "ORCHESTRATOR_OIDC_CA_CERT {} contains invalid X.509 certificate {}",
                path.display(),
                index + 1
            ))
        })?;
        if !remaining.is_empty() || !parsed.is_ca() {
            return Err(OidcConfigurationError::Invalid(format!(
                "ORCHESTRATOR_OIDC_CA_CERT {} certificate {} is not a valid CA certificate",
                path.display(),
                index + 1
            )));
        }
        roots.add(der.clone()).map_err(|error| {
            OidcConfigurationError::Invalid(format!(
                "ORCHESTRATOR_OIDC_CA_CERT {} certificate {} is not a usable trust anchor: {error}",
                path.display(),
                index + 1
            ))
        })?;
        certificates.push(Certificate::from_der(der.as_ref()).to_owned());
    }
    if certificates.is_empty() {
        return Err(OidcConfigurationError::Invalid(format!(
            "ORCHESTRATOR_OIDC_CA_CERT {} contains no CA certificates",
            path.display()
        )));
    }
    Ok(OidcCaBundle {
        source: path.to_path_buf(),
        certificates,
    })
}

fn required_env(name: &str) -> Result<String, OidcConfigurationError> {
    optional_env(name).ok_or_else(|| {
        OidcConfigurationError::Invalid(format!("production PostgreSQL mode requires {name}"))
    })
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn duration_env(
    name: &str,
    default: Duration,
    minimum_seconds: u64,
    maximum_seconds: u64,
) -> Result<Duration, OidcConfigurationError> {
    let Some(value) = optional_env(name) else {
        return Ok(default);
    };
    let seconds = value.parse::<u64>().map_err(|_| {
        OidcConfigurationError::Invalid(format!("{name} must be an integer number of seconds"))
    })?;
    if !(minimum_seconds..=maximum_seconds).contains(&seconds) {
        return Err(OidcConfigurationError::Invalid(format!(
            "{name} must be between {minimum_seconds} and {maximum_seconds} seconds"
        )));
    }
    Ok(Duration::from_secs(seconds))
}
