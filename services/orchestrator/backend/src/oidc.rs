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

    #[cfg(test)]
    fn for_test(issuer: String, audience: &str) -> Self {
        Self::new(
            issuer,
            audience.to_string(),
            DEFAULT_ROLE_CLAIM.to_string(),
            [
                "viewer".to_string(),
                "operator".to_string(),
                "admin".to_string(),
            ],
            Duration::from_secs(3600),
            Duration::from_secs(2),
            true,
            None,
        )
        .expect("test OIDC config")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::tempdir;

    #[derive(Clone)]
    struct TestKey {
        kid: String,
        private_der: Vec<u8>,
        modulus: &'static str,
        exponent: &'static str,
    }

    impl TestKey {
        fn generate(kid: &str) -> Self {
            Self {
                kid: kid.to_string(),
                // Fixed PKCS#1 DER fixtures keep the RS256/JWKS tests real
                // without pulling the vulnerable rsa crate into dev builds.
                // These keys are test-only and must never be used by runtime code.
                private_der: STANDARD
                    .decode(include_str!("testdata/oidc_rsa_primary.der.b64").trim())
                    .expect("decode primary RSA test fixture"),
                modulus: "rdx5Wpz9_SkaLmgisK9Sy_qGbXpC2BrmRCet8aO6_CsA-HA2LWaQzzW5AqISSK00PK3U6lcLKZRmlH5I2XSFEUl83MxNhtvhXgZ15pguZDfahfkfVw8I6EQrjocZplYpbHc6MNHj9ZppK4Vvp4QGoKEzDt58M5EZIL4fAOWChesJqA42MbFoHv8YRTtzckhFHmbh2aJyK358K_AjHms-O_RM-naSkX80poUp9w2MoOpk_bp15roS4gDWCM4wSH0SzClOp2eSQsLhvvSmRa_-ZusMk8_VkC5whUGQ9ufbKCMJwjo6rnOZNqUEbzv8xqMBol-1NHPF7c0-Lxx6DJYN6Q",
                exponent: "AQAB",
            }
        }

        fn rotated(kid: &str) -> Self {
            Self {
                kid: kid.to_string(),
                private_der: STANDARD
                    .decode(include_str!("testdata/oidc_rsa_secondary.der.b64").trim())
                    .expect("decode secondary RSA test fixture"),
                modulus: "wTVBxkQpxleLZfibIK6zx_TPbtEzYIdg1qltqEg7sKeTwguULfv22Hp5g5we8Wc2Sz_ZXShu9XO93iPmV1fto-uIvdUb9jlPPJ_2ak4vg_mq4ATEdxlMUEieA-mwzoCFxBLAuGRZ0iSczGtoXjBJkhXRH8ZjTwZBhBvHn_Gget7OFwCxrURhs45_t_P132ZL5SOxtB-VQAFKRnoJ682y_0reF56gVndOdDm-5p2jBL2KBPg8fO-elQOkFaJ2ebTl3EWOaaullRurMhKrirrBs1wwThV2Y-LV7AB3OTQxis13_G7w6PDjkPkFXiK_cUGTOjFBw9oyOjiYF_Y7XkMszQ",
                exponent: "AQAB",
            }
        }

        fn jwk(&self) -> Value {
            json!({
                "kty": "RSA",
                "kid": self.kid,
                "use": "sig",
                "key_ops": ["verify"],
                "alg": "RS256",
                "n": self.modulus,
                "e": self.exponent,
            })
        }

        fn sign(&self, claims: &Value) -> String {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some(self.kid.clone());
            encode(
                &header,
                claims,
                &EncodingKey::from_rsa_der(&self.private_der),
            )
            .expect("sign test JWT")
        }
    }

    struct MockOidc {
        origin: String,
        handle: JoinHandle<()>,
    }

    impl MockOidc {
        fn spawn(
            build_responses: impl FnOnce(&str) -> Vec<(&'static str, Value)> + Send + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock OIDC server");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let responses = build_responses(&origin);
            let handle = thread::spawn(move || {
                for (expected_path, body) in responses {
                    let (mut stream, _) = listener.accept().expect("accept OIDC request");
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .expect("set OIDC mock timeout");
                    let (request, request_body) = read_request(&mut stream);
                    let request_line = request.lines().next().unwrap_or_default();
                    let expected_method = if expected_path == "/token" {
                        "POST"
                    } else {
                        "GET"
                    };
                    assert!(
                        request_line
                            .starts_with(&format!("{expected_method} {expected_path} HTTP/1.1")),
                        "unexpected OIDC request: {request_line}"
                    );
                    if expected_path == "/token" {
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("content-type: application/x-www-form-urlencoded")
                        );
                        assert!(request_body.contains("grant_type=authorization_code"));
                        assert!(request_body.contains("code=authorization-code"));
                        assert!(request_body.contains("code_verifier=pkce-verifier"));
                        assert!(request_body.contains("client_id=orchestrator-web"));
                    }
                    let body = serde_json::to_vec(&body).expect("encode mock OIDC response");
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .expect("write OIDC headers");
                    stream.write_all(&body).expect("write OIDC body");
                    let _ = stream.shutdown(Shutdown::Both);
                }
            });
            Self { origin, handle }
        }

        fn finish(self) {
            self.handle.join().expect("join mock OIDC server");
        }
    }

    fn read_request(stream: &mut impl Read) -> (String, String) {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 512];
        let header_end = loop {
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
            let count = stream.read(&mut chunk).expect("read OIDC request");
            assert!(count > 0, "OIDC client closed before sending headers");
            bytes.extend_from_slice(&chunk[..count]);
            assert!(bytes.len() <= 16 * 1024, "OIDC request headers too large");
        };
        let headers = String::from_utf8(bytes[..header_end].to_vec())
            .expect("OIDC request headers are UTF-8");
        let content_length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let count = stream.read(&mut chunk).expect("read OIDC request body");
            assert!(count > 0, "OIDC client closed before sending body");
            bytes.extend_from_slice(&chunk[..count]);
        }
        let body = String::from_utf8(bytes[header_end..header_end + content_length].to_vec())
            .expect("OIDC request body is UTF-8");
        (headers, body)
    }

    fn test_oidc_tls_material() -> (String, Arc<rustls::ServerConfig>) {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "OJOS OIDC test CA");
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

    struct MockHttpsOidc {
        origin: String,
        ca_pem: String,
        handle: JoinHandle<()>,
    }

    impl MockHttpsOidc {
        fn spawn(key: TestKey) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTPS OIDC server");
            let origin = format!(
                "https://localhost:{}",
                listener.local_addr().unwrap().port()
            );
            let responses = vec![
                ("/.well-known/openid-configuration", discovery(&origin)),
                ("/jwks", jwks(&[&key])),
            ];
            let (ca_pem, server_config) = test_oidc_tls_material();
            let handle = thread::spawn(move || {
                for (expected_path, body) in responses {
                    let (tcp, _) = listener.accept().expect("accept HTTPS OIDC request");
                    tcp.set_read_timeout(Some(Duration::from_secs(3)))
                        .expect("set HTTPS OIDC timeout");
                    let connection = rustls::ServerConnection::new(server_config.clone())
                        .expect("create HTTPS OIDC connection");
                    let mut stream = rustls::StreamOwned::new(connection, tcp);
                    let (request, _) = read_request(&mut stream);
                    let request_line = request.lines().next().unwrap_or_default();
                    assert!(
                        request_line.starts_with(&format!("GET {expected_path} HTTP/1.1")),
                        "unexpected HTTPS OIDC request: {request_line}"
                    );
                    let body = serde_json::to_vec(&body).expect("encode HTTPS OIDC response");
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .expect("write HTTPS OIDC headers");
                    stream.write_all(&body).expect("write HTTPS OIDC body");
                    stream.flush().expect("flush HTTPS OIDC response");
                }
            });
            Self {
                origin,
                ca_pem,
                handle,
            }
        }

        fn finish(self) {
            self.handle.join().expect("join HTTPS OIDC server");
        }
    }

    fn discovery(origin: &str) -> Value {
        json!({
            "issuer": origin,
            "jwks_uri": format!("{origin}/jwks"),
            "id_token_signing_alg_values_supported": ["RS256"],
        })
    }

    fn interactive_discovery(origin: &str) -> Value {
        json!({
            "issuer": origin,
            "jwks_uri": format!("{origin}/jwks"),
            "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "id_token_signing_alg_values_supported": ["RS256"],
        })
    }

    fn jwks(keys: &[&TestKey]) -> Value {
        json!({ "keys": keys.iter().map(|key| key.jwk()).collect::<Vec<_>>() })
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_secs()
    }

    fn claims(issuer: &str, audience: &str, role: Option<Value>) -> Value {
        let mut claims = json!({
            "iss": issuer,
            "aud": audience,
            "sub": "user-123",
            "exp": now() + 300,
            "nbf": now().saturating_sub(1),
        });
        if let Some(role) = role {
            claims[DEFAULT_ROLE_CLAIM] = role;
        }
        claims
    }

    fn verifier_with_key(key: &TestKey) -> (MockOidc, OidcVerifier) {
        let key = key.clone();
        let server = MockOidc::spawn(move |origin| {
            vec![
                ("/.well-known/openid-configuration", discovery(origin)),
                ("/jwks", jwks(&[&key])),
            ]
        });
        let verifier = OidcVerifier::discover(OidcConfig::for_test(
            server.origin.clone(),
            "orchestrator-api",
        ))
        .expect("discover mock OIDC provider");
        (server, verifier)
    }

    fn interactive_verifier(key: &TestKey, id_nonce: &str) -> (MockOidc, OidcVerifier) {
        interactive_verifier_with_id_token(key, id_nonce, None, "orchestrator-web", "user-123")
    }

    fn interactive_verifier_with_id_token(
        key: &TestKey,
        id_nonce: &str,
        id_issuer: Option<&str>,
        id_audience: &str,
        id_subject: &str,
    ) -> (MockOidc, OidcVerifier) {
        let key = key.clone();
        let id_nonce = id_nonce.to_string();
        let id_issuer = id_issuer.map(str::to_string);
        let id_audience = id_audience.to_string();
        let id_subject = id_subject.to_string();
        let server = MockOidc::spawn(move |origin| {
            let access_token = key.sign(&claims(origin, "orchestrator-api", Some(json!("admin"))));
            let id_token = key.sign(&json!({
                "iss": id_issuer.as_deref().unwrap_or(origin),
                "aud": id_audience,
                "sub": id_subject,
                "exp": now() + 300,
                "nonce": id_nonce,
            }));
            vec![
                (
                    "/.well-known/openid-configuration",
                    interactive_discovery(origin),
                ),
                ("/jwks", jwks(&[&key])),
                (
                    "/token",
                    json!({
                        "access_token": access_token,
                        "token_type": "Bearer",
                        "id_token": id_token,
                        "expires_in": 300,
                    }),
                ),
            ]
        });
        let verifier = OidcVerifier::discover(OidcConfig::for_test(
            server.origin.clone(),
            "orchestrator-api",
        ))
        .expect("discover interactive mock OIDC provider");
        (server, verifier)
    }

    #[test]
    fn discovery_and_jwks_validate_signed_principals_and_default_to_viewer() {
        let key = TestKey::generate("current-key");
        let (server, mut verifier) = verifier_with_key(&key);

        let operator = key.sign(&claims(
            &server.origin,
            "orchestrator-api",
            Some(json!(["viewer", "operator"])),
        ));
        let principal = verifier
            .verify_bearer(Some(&format!("Bearer {operator}")))
            .unwrap()
            .unwrap();
        assert_eq!(principal.id(), "user-123");
        assert_eq!(principal.role(), V1Role::Operator);
        assert_eq!(principal.source(), PrincipalSource::Oidc);

        let minimum = key.sign(&claims(&server.origin, "orchestrator-api", None));
        assert_eq!(
            verifier
                .verify_bearer(Some(&format!("Bearer {minimum}")))
                .unwrap()
                .unwrap()
                .role(),
            V1Role::Viewer
        );

        verifier.config.role_claim = "realm_access.roles".to_string();
        let nested = key.sign(&json!({
            "iss": server.origin.clone(),
            "aud": "orchestrator-api",
            "sub": "nested-role-user",
            "exp": now() + 300,
            "realm_access": { "roles": ["admin"] },
        }));
        assert_eq!(
            verifier
                .verify_bearer(Some(&format!("Bearer {nested}")))
                .unwrap()
                .unwrap()
                .role(),
            V1Role::Admin
        );
        server.finish();
    }

    #[test]
    fn unknown_kid_forces_one_jwks_rotation_refresh() {
        let old_key = TestKey::generate("old-key");
        let new_key = TestKey::generate("new-key");
        let old_response_key = old_key.clone();
        let new_response_key = new_key.clone();
        let server = MockOidc::spawn(move |origin| {
            vec![
                ("/.well-known/openid-configuration", discovery(origin)),
                ("/jwks", jwks(&[&old_response_key])),
                ("/jwks", jwks(&[&new_response_key])),
            ]
        });
        let verifier = OidcVerifier::discover(OidcConfig::for_test(
            server.origin.clone(),
            "orchestrator-api",
        ))
        .expect("discover mock OIDC provider");
        let token = new_key.sign(&claims(
            &server.origin,
            "orchestrator-api",
            Some(json!("admin")),
        ));
        assert_eq!(
            verifier
                .verify_bearer(Some(&format!("Bearer {token}")))
                .unwrap()
                .unwrap()
                .role(),
            V1Role::Admin
        );
        server.finish();
    }

    #[test]
    fn invalid_signature_forces_same_kid_jwks_rotation_refresh() {
        let old_key = TestKey::generate("rotating-key");
        let new_key = TestKey::rotated("rotating-key");
        let old_response_key = old_key.clone();
        let new_response_key = new_key.clone();
        let server = MockOidc::spawn(move |origin| {
            vec![
                ("/.well-known/openid-configuration", discovery(origin)),
                ("/jwks", jwks(&[&old_response_key])),
                ("/jwks", jwks(&[&new_response_key])),
            ]
        });
        let verifier = OidcVerifier::discover(OidcConfig::for_test(
            server.origin.clone(),
            "orchestrator-api",
        ))
        .expect("discover mock OIDC provider");
        let token = new_key.sign(&claims(
            &server.origin,
            "orchestrator-api",
            Some(json!("operator")),
        ));

        assert_eq!(
            verifier
                .verify_bearer(Some(&format!("Bearer {token}")))
                .unwrap()
                .unwrap()
                .role(),
            V1Role::Operator
        );
        server.finish();
    }

    #[test]
    fn wrong_issuer_audience_expiry_nbf_and_unknown_role_are_rejected() {
        let key = TestKey::generate("validation-key");
        let (server, verifier) = verifier_with_key(&key);

        let wrong_issuer = key.sign(&claims(
            "https://wrong-issuer.example",
            "orchestrator-api",
            Some(json!("viewer")),
        ));
        assert!(
            verifier
                .verify_bearer(Some(&format!("Bearer {wrong_issuer}")))
                .is_err()
        );

        let wrong_audience = key.sign(&claims(
            &server.origin,
            "some-other-api",
            Some(json!("viewer")),
        ));
        assert!(
            verifier
                .verify_bearer(Some(&format!("Bearer {wrong_audience}")))
                .is_err()
        );

        let mut expired = claims(&server.origin, "orchestrator-api", Some(json!("viewer")));
        expired["exp"] = json!(now().saturating_sub(30));
        let expired = key.sign(&expired);
        assert!(
            verifier
                .verify_bearer(Some(&format!("Bearer {expired}")))
                .is_err()
        );

        let mut immature = claims(&server.origin, "orchestrator-api", Some(json!("viewer")));
        immature["nbf"] = json!(now() + 300);
        let immature = key.sign(&immature);
        assert!(
            verifier
                .verify_bearer(Some(&format!("Bearer {immature}")))
                .is_err()
        );

        let unknown_role = key.sign(&claims(
            &server.origin,
            "orchestrator-api",
            Some(json!("superuser")),
        ));
        assert!(
            verifier
                .verify_bearer(Some(&format!("Bearer {unknown_role}")))
                .unwrap_err()
                .to_string()
                .contains("unmapped")
        );
        server.finish();
    }

    #[test]
    fn authorization_code_exchange_validates_access_token_id_token_nonce_and_pkce() {
        let key = TestKey::generate("code-flow-key");
        let (server, verifier) = interactive_verifier(&key, "expected-nonce");
        let identity = verifier
            .exchange_authorization_code(
                "authorization-code",
                "pkce-verifier",
                "https://orchestrator.example/api/v1/auth/oidc/callback",
                "orchestrator-web",
                "expected-nonce",
            )
            .expect("valid Authorization Code + PKCE exchange");
        assert_eq!(identity.principal.id(), "user-123");
        assert_eq!(identity.principal.role(), V1Role::Admin);
        assert!(
            identity
                .expires_in
                .is_some_and(|ttl| (1..=300).contains(&ttl))
        );
        server.finish();

        let (server, verifier) = interactive_verifier(&key, "attacker-nonce");
        let error = verifier
            .exchange_authorization_code(
                "authorization-code",
                "pkce-verifier",
                "https://orchestrator.example/api/v1/auth/oidc/callback",
                "orchestrator-web",
                "expected-nonce",
            )
            .unwrap_err();
        assert!(error.to_string().contains("nonce"));
        server.finish();
    }

    #[test]
    fn authorization_code_exchange_rejects_wrong_id_token_issuer_and_audience() {
        let key = TestKey::generate("id-token-validation-key");
        let (server, verifier) = interactive_verifier_with_id_token(
            &key,
            "expected-nonce",
            None,
            "attacker-client",
            "user-123",
        );
        let error = verifier
            .exchange_authorization_code(
                "authorization-code",
                "pkce-verifier",
                "https://orchestrator.example/api/v1/auth/oidc/callback",
                "orchestrator-web",
                "expected-nonce",
            )
            .unwrap_err();
        assert!(error.to_string().to_ascii_lowercase().contains("audience"));
        server.finish();

        let (server, verifier) = interactive_verifier_with_id_token(
            &key,
            "expected-nonce",
            Some("https://attacker.invalid"),
            "orchestrator-web",
            "user-123",
        );
        let error = verifier
            .exchange_authorization_code(
                "authorization-code",
                "pkce-verifier",
                "https://orchestrator.example/api/v1/auth/oidc/callback",
                "orchestrator-web",
                "expected-nonce",
            )
            .unwrap_err();
        assert!(error.to_string().to_ascii_lowercase().contains("issuer"));
        server.finish();
    }

    #[test]
    fn discovery_issuer_mismatch_and_missing_production_config_fail_closed() {
        let server = MockOidc::spawn(|origin| {
            vec![(
                "/.well-known/openid-configuration",
                json!({
                    "issuer": "https://attacker.invalid",
                    "jwks_uri": format!("{origin}/jwks"),
                    "id_token_signing_alg_values_supported": ["RS256"],
                }),
            )]
        });
        let result = OidcVerifier::discover(OidcConfig::for_test(
            server.origin.clone(),
            "orchestrator-api",
        ));
        assert!(matches!(result, Err(OidcConfigurationError::Invalid(_))));
        server.finish();

        let mut env = TestEnv::lock();
        env.remove("ORCHESTRATOR_OIDC_ISSUER");
        env.remove("ORCHESTRATOR_OIDC_AUDIENCE");
        env.remove("ORCHESTRATOR_OIDC_CA_CERT");
        let result = OidcConfig::from_env();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("ORCHESTRATOR_OIDC_ISSUER")
        );

        env.set("ORCHESTRATOR_OIDC_ISSUER", "http://127.0.0.1:8080");
        env.set("ORCHESTRATOR_OIDC_AUDIENCE", "orchestrator-api");
        assert!(
            OidcConfig::from_env()
                .unwrap_err()
                .to_string()
                .contains("must use HTTPS")
        );
    }

    #[test]
    fn explicit_oidc_ca_bundle_enables_verified_private_https_discovery() {
        let key = TestKey::generate("private-ca-key");
        let server = MockHttpsOidc::spawn(key);
        let directory = tempdir().unwrap();
        let ca_path = directory.path().join("oidc-ca.pem");
        std::fs::write(&ca_path, &server.ca_pem).unwrap();

        let mut env = TestEnv::lock();
        env.set("ORCHESTRATOR_OIDC_ISSUER", &server.origin);
        env.set("ORCHESTRATOR_OIDC_AUDIENCE", "orchestrator-api");
        env.set(
            "ORCHESTRATOR_OIDC_CA_CERT",
            ca_path.to_str().expect("temporary path is Unicode"),
        );
        let config = OidcConfig::from_env().expect("load explicit OIDC CA bundle");
        assert_eq!(
            config
                .ca_bundle
                .as_ref()
                .expect("explicit CA bundle")
                .certificates
                .len(),
            1
        );

        let verifier = OidcVerifier::discover(config)
            .expect("private HTTPS OIDC discovery succeeds with explicit CA");
        let tls = verifier.agent.config().tls_config();
        assert!(tls.use_sni());
        assert!(!tls.disable_verification());
        match tls.root_certs() {
            RootCerts::Specific(certificates) => assert_eq!(certificates.len(), 1),
            roots => panic!("expected explicit OIDC roots, received {roots:?}"),
        }
        server.finish();
    }

    #[test]
    fn explicit_oidc_ca_bundle_configuration_fails_closed() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("missing-ca.pem");
        let empty = directory.path().join("empty-ca.pem");
        let malformed = directory.path().join("malformed-ca.pem");
        let not_ca = directory.path().join("server.pem");
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(
            &malformed,
            b"-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let server_key = KeyPair::generate().unwrap();
        let server_certificate = CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .self_signed(&server_key)
            .unwrap();
        std::fs::write(&not_ca, server_certificate.pem()).unwrap();

        let mut env = TestEnv::lock();
        env.set(
            "ORCHESTRATOR_OIDC_ISSUER",
            "https://identity.example.invalid",
        );
        env.set("ORCHESTRATOR_OIDC_AUDIENCE", "orchestrator-api");

        env.set("ORCHESTRATOR_OIDC_CA_CERT", "");
        assert!(
            OidcConfig::from_env()
                .unwrap_err()
                .to_string()
                .contains("non-empty absolute path")
        );

        for (path, expected) in [
            (&missing, "read ORCHESTRATOR_OIDC_CA_CERT"),
            (&empty, "contains no CA certificates"),
            (&malformed, "contains invalid PEM"),
            (&not_ca, "is not a valid CA certificate"),
        ] {
            env.set(
                "ORCHESTRATOR_OIDC_CA_CERT",
                path.to_str().expect("temporary path is Unicode"),
            );
            let error = OidcConfig::from_env().unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "expected {expected:?} in {error:?}"
            );
        }
    }
}
