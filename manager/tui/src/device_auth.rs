//! OAuth 2.0 Device Authorization Grant for the remote TUI.
//!
//! Tokens returned here are deliberately kept as owned process memory and are
//! never written to disk or environment variables.

use serde::Deserialize;
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use ureq::Agent;
use ureq::http::Uri;

const MAX_OIDC_RESPONSE_BYTES: usize = 1024 * 1024;
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFlowConfig {
    pub issuer: String,
    pub client_id: String,
    pub scope: String,
    pub audience: Option<String>,
    pub http_timeout: Duration,
}

impl DeviceFlowConfig {
    pub fn new(
        issuer: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Result<Self, DeviceAuthError> {
        let issuer = issuer.into();
        let client_id = client_id.into();
        validate_https_or_loopback(&issuer, "OIDC issuer")?;
        validate_parameter(&client_id, "OIDC client_id", 256)?;
        Ok(Self {
            issuer,
            client_id,
            scope: "openid profile".to_string(),
            audience: None,
            http_timeout: Duration::from_secs(10),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationPrompt {
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    pub expires_in: Duration,
}

#[derive(Debug, Error)]
pub enum DeviceAuthError {
    #[error("invalid OIDC device-flow configuration: {0}")]
    InvalidConfiguration(String),
    #[error("OIDC device-flow transport failed: {0}")]
    Transport(String),
    #[error("OIDC device-flow response is invalid: {0}")]
    InvalidResponse(String),
    #[error("OIDC device authorization was denied: {0}")]
    AccessDenied(String),
    #[error("OIDC device authorization expired")]
    Expired,
    #[error("OIDC provider returned {error}: {description}")]
    Provider { error: String, description: String },
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    device_authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    error_description: String,
}

trait PollWait {
    fn wait(&mut self, duration: Duration);
}

struct ThreadPollWait;

impl PollWait for ThreadPollWait {
    fn wait(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

pub fn authenticate(
    config: &DeviceFlowConfig,
    prompt: impl FnMut(&VerificationPrompt),
) -> Result<String, DeviceAuthError> {
    let mut wait = ThreadPollWait;
    authenticate_with_wait(config, prompt, &mut wait)
}

fn authenticate_with_wait(
    config: &DeviceFlowConfig,
    mut prompt: impl FnMut(&VerificationPrompt),
    wait: &mut impl PollWait,
) -> Result<String, DeviceAuthError> {
    validate_https_or_loopback(&config.issuer, "OIDC issuer")?;
    validate_parameter(&config.client_id, "OIDC client_id", 256)?;
    validate_parameter(&config.scope, "OIDC scope", 1024)?;
    if let Some(audience) = config.audience.as_deref() {
        validate_parameter(audience, "OIDC audience", 512)?;
    }

    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(config.http_timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into();
    let discovery_url = format!(
        "{}/.well-known/openid-configuration",
        config.issuer.trim_end_matches('/')
    );
    let (status, discovery_body) = get(&agent, &discovery_url)?;
    ensure_success(status, &discovery_body, "OIDC discovery")?;
    let discovery: DiscoveryDocument = decode_json(&discovery_body, "OIDC discovery")?;
    if discovery.issuer != config.issuer {
        return Err(DeviceAuthError::InvalidResponse(format!(
            "discovery issuer mismatch: expected {}, received {}",
            config.issuer, discovery.issuer
        )));
    }
    validate_https_or_loopback(
        &discovery.device_authorization_endpoint,
        "device_authorization_endpoint",
    )?;
    validate_https_or_loopback(&discovery.token_endpoint, "token_endpoint")?;

    let mut device_fields = vec![
        ("client_id", config.client_id.as_str()),
        ("scope", config.scope.as_str()),
    ];
    if let Some(audience) = config.audience.as_deref() {
        device_fields.push(("audience", audience));
    }
    let (status, device_body) = post_form(
        &agent,
        &discovery.device_authorization_endpoint,
        &device_fields,
    )?;
    ensure_success(status, &device_body, "device authorization")?;
    let device: DeviceAuthorizationResponse = decode_json(&device_body, "device authorization")?;
    validate_device_response(&device)?;
    let expires_in = Duration::from_secs(device.expires_in);
    prompt(&VerificationPrompt {
        verification_uri: device.verification_uri.clone(),
        verification_uri_complete: device.verification_uri_complete.clone(),
        user_code: device.user_code.clone(),
        expires_in,
    });

    let deadline = Instant::now()
        .checked_add(expires_in)
        .ok_or(DeviceAuthError::Expired)?;
    let mut interval = Duration::from_secs(device.interval);
    loop {
        if Instant::now()
            .checked_add(interval)
            .is_none_or(|next| next >= deadline)
        {
            return Err(DeviceAuthError::Expired);
        }
        wait.wait(interval);
        let (status, token_body) = post_form(
            &agent,
            &discovery.token_endpoint,
            &[
                ("grant_type", DEVICE_GRANT_TYPE),
                ("device_code", device.device_code.as_str()),
                ("client_id", config.client_id.as_str()),
            ],
        )?;
        if (200..300).contains(&status) {
            let token: TokenResponse = decode_json(&token_body, "token")?;
            if !token.token_type.eq_ignore_ascii_case("bearer") {
                return Err(DeviceAuthError::InvalidResponse(
                    "token_type must be Bearer".to_string(),
                ));
            }
            validate_access_token(&token.access_token)?;
            return Ok(token.access_token);
        }
        let error: TokenErrorResponse = decode_json(&token_body, "token error")?;
        match error.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => {
                interval = interval
                    .checked_add(Duration::from_secs(5))
                    .unwrap_or(Duration::from_secs(60))
                    .min(Duration::from_secs(60));
            }
            "access_denied" => {
                return Err(DeviceAuthError::AccessDenied(provider_description(&error)));
            }
            "expired_token" => return Err(DeviceAuthError::Expired),
            _ => {
                let description = provider_description(&error);
                return Err(DeviceAuthError::Provider {
                    error: error.error,
                    description,
                });
            }
        }
    }
}

fn validate_device_response(response: &DeviceAuthorizationResponse) -> Result<(), DeviceAuthError> {
    validate_parameter(&response.device_code, "device_code", 4096)?;
    validate_parameter(&response.user_code, "user_code", 256)?;
    validate_https_or_loopback(&response.verification_uri, "verification_uri")?;
    if let Some(uri) = response.verification_uri_complete.as_deref() {
        validate_https_or_loopback(uri, "verification_uri_complete")?;
    }
    if !(1..=3600).contains(&response.expires_in) {
        return Err(DeviceAuthError::InvalidResponse(
            "expires_in must be between 1 and 3600 seconds".to_string(),
        ));
    }
    if !(1..=60).contains(&response.interval) {
        return Err(DeviceAuthError::InvalidResponse(
            "interval must be between 1 and 60 seconds".to_string(),
        ));
    }
    Ok(())
}

fn validate_access_token(token: &str) -> Result<(), DeviceAuthError> {
    if token.is_empty()
        || token.len() > 16 * 1024
        || token
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(DeviceAuthError::InvalidResponse(
            "access_token is empty or not safe for an Authorization header".to_string(),
        ));
    }
    Ok(())
}

fn provider_description(error: &TokenErrorResponse) -> String {
    if error.error_description.trim().is_empty() {
        error.error.clone()
    } else {
        error.error_description.trim().to_string()
    }
}

fn validate_parameter(value: &str, name: &str, max_len: usize) -> Result<(), DeviceAuthError> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > max_len
        || value.chars().any(char::is_control)
    {
        return Err(DeviceAuthError::InvalidConfiguration(format!(
            "{name} must contain 1-{max_len} non-control characters without surrounding whitespace"
        )));
    }
    Ok(())
}

fn validate_https_or_loopback(value: &str, name: &str) -> Result<(), DeviceAuthError> {
    let uri = value.parse::<Uri>().map_err(|error| {
        DeviceAuthError::InvalidConfiguration(format!("{name} is not a valid URL: {error}"))
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
        return Err(DeviceAuthError::InvalidConfiguration(format!(
            "{name} must be HTTPS (HTTP is allowed only for loopback testing) and contain no credentials"
        )));
    }
    Ok(())
}

fn get(agent: &Agent, url: &str) -> Result<(u16, Vec<u8>), DeviceAuthError> {
    let response = agent
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|error| DeviceAuthError::Transport(error.to_string()))?;
    read_response(response)
}

fn post_form(
    agent: &Agent,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<(u16, Vec<u8>), DeviceAuthError> {
    let body = fields
        .iter()
        .map(|(name, value)| format!("{}={}", form_component(name), form_component(value)))
        .collect::<Vec<_>>()
        .join("&");
    let response = agent
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body.as_bytes())
        .map_err(|error| DeviceAuthError::Transport(error.to_string()))?;
    read_response(response)
}

fn read_response(
    response: ureq::http::Response<ureq::Body>,
) -> Result<(u16, Vec<u8>), DeviceAuthError> {
    let status = response.status().as_u16();
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_OIDC_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| DeviceAuthError::Transport(error.to_string()))?;
    if body.len() > MAX_OIDC_RESPONSE_BYTES {
        return Err(DeviceAuthError::InvalidResponse(format!(
            "response exceeds {MAX_OIDC_RESPONSE_BYTES} bytes"
        )));
    }
    Ok((status, body))
}

fn ensure_success(status: u16, body: &[u8], context: &str) -> Result<(), DeviceAuthError> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(body);
    Err(DeviceAuthError::Provider {
        error: format!("http_{status}"),
        description: format!("{context} failed: {}", detail.trim()),
    })
}

fn decode_json<T: for<'de> Deserialize<'de>>(
    body: &[u8],
    context: &str,
) -> Result<T, DeviceAuthError> {
    serde_json::from_slice(body).map_err(|error| {
        DeviceAuthError::InvalidResponse(format!("{context} JSON is invalid: {error}"))
    })
}

fn form_component(value: &str) -> String {
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
