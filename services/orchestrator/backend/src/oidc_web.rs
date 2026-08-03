//! Browser OIDC Authorization Code + PKCE and server-side Web sessions.

use crate::auth::Principal;
use crate::oidc::{OidcVerifier, url_encode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use getrandom::fill;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;
use ureq::http::Uri;

pub(crate) const OIDC_SESSION_COOKIE_NAME: &str = "ojos_oidc_session";
pub(crate) const CSRF_HEADER: &str = "x-csrf-token";
const DEFAULT_STATE_TTL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_PENDING_STATES: usize = 1_024;
const MAX_SESSIONS: usize = 10_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum OidcWebError {
    #[error("{0}")]
    Configuration(String),
    #[error("OIDC authorization state is invalid, expired, or already used")]
    InvalidState,
    #[error("OIDC authorization code is missing")]
    MissingCode,
    #[error("OIDC Web session is invalid or expired")]
    InvalidSession,
    #[error("OIDC Web session CSRF token is missing or invalid")]
    InvalidCsrf,
    #[error("OIDC browser session capacity is exhausted")]
    Capacity,
    #[error("failed to generate OIDC browser entropy")]
    Entropy,
    #[error("OIDC browser session lock poisoned")]
    Poisoned,
    #[error("OIDC token validation failed: {0}")]
    Verification(String),
}

#[derive(Debug, Clone)]
pub(crate) struct OidcBrowserConfig {
    pub(crate) issuer: String,
    pub(crate) audience: String,
    pub(crate) client_id: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) authorization_endpoint: String,
    pub(crate) redirect_uri: String,
}

impl OidcBrowserConfig {
    pub(crate) fn from_env(verifier: &OidcVerifier) -> Result<Self, OidcWebError> {
        let client_id = required_env("ORCHESTRATOR_OIDC_CLIENT_ID")?;
        validate_simple_value(&client_id, "ORCHESTRATOR_OIDC_CLIENT_ID")?;
        let public_base_url = required_env("ORCHESTRATOR_PUBLIC_BASE_URL")?;
        validate_public_base_url(&public_base_url, false)?;
        let scope_text = std::env::var("ORCHESTRATOR_OIDC_SCOPES")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "openid profile email".to_string());
        let scopes = validate_scopes(&scope_text)?;
        let authorization_endpoint = verifier
            .authorization_endpoint()
            .ok_or_else(|| {
                OidcWebError::Configuration(
                    "OIDC discovery must advertise authorization_endpoint".to_string(),
                )
            })?
            .to_string();
        if verifier.token_endpoint().is_none() {
            return Err(OidcWebError::Configuration(
                "OIDC discovery must advertise token_endpoint".to_string(),
            ));
        }
        Ok(Self {
            issuer: verifier.issuer().to_string(),
            audience: verifier.audience().to_string(),
            client_id,
            scopes,
            authorization_endpoint,
            redirect_uri: format!(
                "{}/api/v1/auth/oidc/callback",
                public_base_url.trim_end_matches('/')
            ),
        })
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self {
            issuer: "https://issuer.example".to_string(),
            audience: "orchestrator-api".to_string(),
            client_id: "orchestrator-web".to_string(),
            scopes: vec!["openid".to_string(), "profile".to_string()],
            authorization_endpoint: "https://issuer.example/authorize".to_string(),
            redirect_uri: "https://orchestrator.example/api/v1/auth/oidc/callback".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizationStart {
    pub(crate) location: String,
}

#[derive(Debug, Clone)]
struct PendingAuthorization {
    code_verifier: String,
    nonce: String,
    return_to: String,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct BrowserSession {
    principal: Principal,
    csrf_token: String,
    csrf_hash: [u8; 32],
    expires_at: Instant,
}

#[derive(Debug, Default)]
struct State {
    pending: HashMap<[u8; 32], PendingAuthorization>,
    sessions: HashMap<[u8; 32], BrowserSession>,
}

#[derive(Debug, Clone)]
pub(crate) struct BrowserSessionView {
    pub(crate) principal: Principal,
    pub(crate) csrf_token: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthorizationCompletion {
    pub(crate) session_id: String,
    pub(crate) principal: Principal,
    pub(crate) return_to: String,
    pub(crate) max_age_seconds: u64,
}

#[derive(Debug)]
pub(crate) struct OidcWebSessionManager {
    config: OidcBrowserConfig,
    state_ttl: Duration,
    session_ttl: Duration,
    state: Mutex<State>,
}

impl OidcWebSessionManager {
    pub(crate) fn new(config: OidcBrowserConfig) -> Self {
        Self::with_ttls(config, DEFAULT_STATE_TTL, DEFAULT_SESSION_TTL)
    }

    fn with_ttls(config: OidcBrowserConfig, state_ttl: Duration, session_ttl: Duration) -> Self {
        Self {
            config,
            state_ttl,
            session_ttl,
            state: Mutex::new(State::default()),
        }
    }

    pub(crate) fn config(&self) -> &OidcBrowserConfig {
        &self.config
    }

    pub(crate) fn begin(
        &self,
        return_to: Option<&str>,
    ) -> Result<AuthorizationStart, OidcWebError> {
        let return_to = valid_return_to(return_to.unwrap_or("/"))?;
        let state_token = random_token(32)?;
        let nonce = random_token(32)?;
        let code_verifier = random_token(64)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let mut state = self.state.lock().map_err(|_| OidcWebError::Poisoned)?;
        purge_expired(&mut state);
        if state.pending.len() >= MAX_PENDING_STATES {
            return Err(OidcWebError::Capacity);
        }
        state.pending.insert(
            digest(&state_token),
            PendingAuthorization {
                code_verifier,
                nonce: nonce.clone(),
                return_to,
                expires_at: Instant::now() + self.state_ttl,
            },
        );
        let scope = self.config.scopes.join(" ");
        let query = [
            ("response_type", "code"),
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", self.config.redirect_uri.as_str()),
            ("scope", scope.as_str()),
            ("state", state_token.as_str()),
            ("nonce", nonce.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]
        .iter()
        .map(|(name, value)| format!("{}={}", url_encode(name), url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
        Ok(AuthorizationStart {
            location: format!("{}?{}", self.config.authorization_endpoint, query),
        })
    }

    pub(crate) fn complete(
        &self,
        verifier: &OidcVerifier,
        state_token: Option<&str>,
        code: Option<&str>,
    ) -> Result<AuthorizationCompletion, OidcWebError> {
        let state_token = state_token
            .filter(|value| !value.is_empty())
            .ok_or(OidcWebError::InvalidState)?;
        // Consume before network I/O. A failed or interrupted exchange can never replay a code.
        let pending = self.consume_pending(state_token)?;
        let code = code
            .filter(|value| !value.is_empty())
            .ok_or(OidcWebError::MissingCode)?;
        let identity = verifier
            .exchange_authorization_code(
                code,
                &pending.code_verifier,
                &self.config.redirect_uri,
                &self.config.client_id,
                &pending.nonce,
            )
            .map_err(|error| OidcWebError::Verification(error.to_string()))?;
        let ttl = identity
            .expires_in
            .map(Duration::from_secs)
            .map(|token_ttl| token_ttl.min(self.session_ttl))
            .unwrap_or(self.session_ttl);
        self.issue_session(identity.principal, pending.return_to, ttl)
    }

    pub(crate) fn reject(&self, state_token: Option<&str>) -> Result<(), OidcWebError> {
        let state_token = state_token
            .filter(|value| !value.is_empty())
            .ok_or(OidcWebError::InvalidState)?;
        self.consume_pending(state_token).map(|_| ())
    }

    fn consume_pending(&self, state_token: &str) -> Result<PendingAuthorization, OidcWebError> {
        let mut state = self.state.lock().map_err(|_| OidcWebError::Poisoned)?;
        purge_expired(&mut state);
        state
            .pending
            .remove(&digest(state_token))
            .filter(|pending| pending.expires_at > Instant::now())
            .ok_or(OidcWebError::InvalidState)
    }

    fn issue_session(
        &self,
        principal: Principal,
        return_to: String,
        ttl: Duration,
    ) -> Result<AuthorizationCompletion, OidcWebError> {
        if ttl.is_zero() {
            return Err(OidcWebError::InvalidSession);
        }
        let session_id = random_token(32)?;
        let csrf_token = random_token(32)?;
        let mut state = self.state.lock().map_err(|_| OidcWebError::Poisoned)?;
        purge_expired(&mut state);
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(OidcWebError::Capacity);
        }
        state.sessions.insert(
            digest(&session_id),
            BrowserSession {
                principal: principal.clone(),
                csrf_hash: digest(&csrf_token),
                csrf_token: csrf_token.clone(),
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(AuthorizationCompletion {
            session_id,
            principal,
            return_to,
            max_age_seconds: ttl.as_secs(),
        })
    }

    pub(crate) fn authorize(
        &self,
        cookie_header: Option<&str>,
        csrf_header: Option<&str>,
        mutation: bool,
    ) -> Result<Option<BrowserSessionView>, OidcWebError> {
        let Some(session_id) = cookie_value(cookie_header, OIDC_SESSION_COOKIE_NAME) else {
            return Ok(None);
        };
        let key = digest(session_id);
        let mut state = self.state.lock().map_err(|_| OidcWebError::Poisoned)?;
        purge_expired(&mut state);
        let session = state
            .sessions
            .get(&key)
            .ok_or(OidcWebError::InvalidSession)?;
        if mutation {
            let csrf = csrf_header.ok_or(OidcWebError::InvalidCsrf)?;
            if !constant_time_eq(&session.csrf_hash, &digest(csrf)) {
                return Err(OidcWebError::InvalidCsrf);
            }
        }
        Ok(Some(BrowserSessionView {
            principal: session.principal.clone(),
            csrf_token: session.csrf_token.clone(),
        }))
    }

    pub(crate) fn logout(
        &self,
        cookie_header: Option<&str>,
        csrf_header: Option<&str>,
    ) -> Result<(), OidcWebError> {
        let Some(session_id) = cookie_value(cookie_header, OIDC_SESSION_COOKIE_NAME) else {
            return Err(OidcWebError::InvalidSession);
        };
        let key = digest(session_id);
        let mut state = self.state.lock().map_err(|_| OidcWebError::Poisoned)?;
        purge_expired(&mut state);
        let session = state
            .sessions
            .get(&key)
            .ok_or(OidcWebError::InvalidSession)?;
        let csrf = csrf_header.ok_or(OidcWebError::InvalidCsrf)?;
        if !constant_time_eq(&session.csrf_hash, &digest(csrf)) {
            return Err(OidcWebError::InvalidCsrf);
        }
        state.sessions.remove(&key);
        Ok(())
    }
}

pub(crate) fn session_cookie(session_id: &str, max_age_seconds: u64) -> String {
    format!(
        "{OIDC_SESSION_COOKIE_NAME}={session_id}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={max_age_seconds}"
    )
}

pub(crate) fn expired_session_cookie() -> String {
    format!("{OIDC_SESSION_COOKIE_NAME}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

fn purge_expired(state: &mut State) {
    let now = Instant::now();
    state.pending.retain(|_, pending| pending.expires_at > now);
    state.sessions.retain(|_, session| session.expires_at > now);
}

fn valid_return_to(value: &str) -> Result<String, OidcWebError> {
    if value.is_empty()
        || !value.starts_with('/')
        || value.starts_with("//")
        || value.contains(['\r', '\n'])
        || value.len() > 2_048
    {
        return Err(OidcWebError::Configuration(
            "return_to must be a same-origin absolute path".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn validate_public_base_url(
    value: &str,
    allow_insecure_loopback: bool,
) -> Result<(), OidcWebError> {
    if value.trim() != value || value.ends_with('/') || value.contains(['?', '#']) {
        return Err(OidcWebError::Configuration(
            "ORCHESTRATOR_PUBLIC_BASE_URL must not contain whitespace, query, fragment, or a trailing slash".to_string(),
        ));
    }
    let uri = value.parse::<Uri>().map_err(|error| {
        OidcWebError::Configuration(format!("ORCHESTRATOR_PUBLIC_BASE_URL is invalid: {error}"))
    })?;
    let authority = uri.authority().ok_or_else(|| {
        OidcWebError::Configuration(
            "ORCHESTRATOR_PUBLIC_BASE_URL requires an authority".to_string(),
        )
    })?;
    if authority.as_str().contains('@') {
        return Err(OidcWebError::Configuration(
            "ORCHESTRATOR_PUBLIC_BASE_URL must not contain credentials".to_string(),
        ));
    }
    let secure = uri.scheme_str() == Some("https");
    let test_loopback = allow_insecure_loopback
        && uri.scheme_str() == Some("http")
        && authority
            .host()
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !secure && !test_loopback {
        return Err(OidcWebError::Configuration(
            "ORCHESTRATOR_PUBLIC_BASE_URL must use HTTPS".to_string(),
        ));
    }
    Ok(())
}

fn validate_simple_value(value: &str, name: &str) -> Result<(), OidcWebError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_whitespace) {
        return Err(OidcWebError::Configuration(format!(
            "{name} must be a non-empty value without whitespace"
        )));
    }
    Ok(())
}

fn validate_scopes(value: &str) -> Result<Vec<String>, OidcWebError> {
    let scopes = value
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if scopes.is_empty()
        || scopes.len() > 16
        || !scopes.iter().any(|scope| scope == "openid")
        || scopes.iter().any(|scope| {
            scope.len() > 128
                || !scope.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
                })
        })
    {
        return Err(OidcWebError::Configuration(
            "ORCHESTRATOR_OIDC_SCOPES must contain openid and at most 16 simple scopes".to_string(),
        ));
    }
    Ok(scopes)
}

fn required_env(name: &str) -> Result<String, OidcWebError> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OidcWebError::Configuration(format!("production OIDC Web login requires {name}"))
        })
}

fn random_token(byte_count: usize) -> Result<String, OidcWebError> {
    let mut bytes = vec![0_u8; byte_count];
    fill(&mut bytes).map_err(|_| OidcWebError::Entropy)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn cookie_value<'a>(header: Option<&'a str>, name: &str) -> Option<&'a str> {
    header?.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::PrincipalSource;
    use orchestrator_legacy::V1Role;

    fn state_parameter(location: &str) -> String {
        location
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .find_map(|part| part.strip_prefix("state="))
            .unwrap()
            .to_string()
    }

    #[test]
    fn authorization_start_uses_nonce_state_and_pkce_s256_without_exposing_verifier() {
        let manager = OidcWebSessionManager::new(OidcBrowserConfig::for_test());
        let start = manager.begin(Some("/#/nodes")).unwrap();
        assert!(
            start
                .location
                .starts_with("https://issuer.example/authorize?")
        );
        assert!(start.location.contains("response_type=code"));
        assert!(start.location.contains("nonce="));
        assert!(start.location.contains("state="));
        assert!(start.location.contains("code_challenge="));
        assert!(start.location.contains("code_challenge_method=S256"));
        assert!(!start.location.contains("code_verifier"));
        assert!(!start.location.contains("%2F%2Fattacker"));
    }

    #[test]
    fn authorization_state_is_bounded_expiring_and_single_use() {
        let manager = OidcWebSessionManager::with_ttls(
            OidcBrowserConfig::for_test(),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let start = manager.begin(Some("/")).unwrap();
        let state = state_parameter(&start.location);
        let pending = manager.consume_pending(&state).unwrap();
        assert_eq!(pending.return_to, "/");
        assert!(matches!(
            manager.consume_pending(&state),
            Err(OidcWebError::InvalidState)
        ));
        assert!(matches!(
            manager.consume_pending("wrong"),
            Err(OidcWebError::InvalidState)
        ));

        let expired = OidcWebSessionManager::with_ttls(
            OidcBrowserConfig::for_test(),
            Duration::ZERO,
            Duration::from_secs(60),
        );
        let start = expired.begin(None).unwrap();
        assert!(matches!(
            expired.consume_pending(&state_parameter(&start.location)),
            Err(OidcWebError::InvalidState)
        ));
    }

    #[test]
    fn session_is_http_only_and_requires_csrf() {
        let manager = OidcWebSessionManager::new(OidcBrowserConfig::for_test());
        let principal = Principal::verified("user-1", V1Role::Admin, PrincipalSource::Oidc);
        let completion = manager
            .issue_session(principal, "/".to_string(), Duration::from_secs(60))
            .unwrap();
        let cookie = session_cookie(&completion.session_id, completion.max_age_seconds);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        let request_cookie = format!("{OIDC_SESSION_COOKIE_NAME}={}", completion.session_id);
        assert!(
            manager
                .authorize(Some(&request_cookie), None, false)
                .unwrap()
                .is_some()
        );
        let csrf_token = manager
            .authorize(Some(&request_cookie), None, false)
            .unwrap()
            .unwrap()
            .csrf_token;
        assert!(matches!(
            manager.authorize(Some(&request_cookie), None, true),
            Err(OidcWebError::InvalidCsrf)
        ));
        assert!(
            manager
                .authorize(Some(&request_cookie), Some(&csrf_token), true)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn issued_session_expires_server_side() {
        let manager = OidcWebSessionManager::new(OidcBrowserConfig::for_test());
        let principal = Principal::verified("user-1", V1Role::Viewer, PrincipalSource::Oidc);
        let completion = manager
            .issue_session(principal, "/".to_string(), Duration::from_millis(1))
            .unwrap();
        let request_cookie = format!("{OIDC_SESSION_COOKIE_NAME}={}", completion.session_id);
        std::thread::sleep(Duration::from_millis(10));

        assert!(matches!(
            manager.authorize(Some(&request_cookie), None, false),
            Err(OidcWebError::InvalidSession)
        ));
    }

    #[test]
    fn external_return_targets_and_incomplete_scope_config_are_rejected() {
        let manager = OidcWebSessionManager::new(OidcBrowserConfig::for_test());
        assert!(manager.begin(Some("https://attacker.invalid")).is_err());
        assert!(manager.begin(Some("//attacker.invalid")).is_err());
        assert!(validate_scopes("profile email").is_err());
        assert!(validate_public_base_url("http://orchestrator.example", false).is_err());
    }
}
