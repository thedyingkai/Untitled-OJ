//! Server-side adapter from an authenticated Orchestrator principal to Auth's
//! delegated single-permission decision endpoint.

use crate::auth::Principal;
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;
use ureq::Agent;

const EFFECTIVE_PATH_PREFIX: &str = "/auth/admin/users/";
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TIMEOUT: Duration = Duration::from_secs(5);
const TIMEOUT_MS_ENV: &str = "ORCHESTRATOR_AUTH_PERMISSION_TIMEOUT_MS";

#[derive(Clone)]
pub(crate) struct AuthPermissionChecker {
    url: String,
    bearer_token: String,
    agent: Agent,
}

impl std::fmt::Debug for AuthPermissionChecker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthPermissionChecker")
            .field("url", &self.url)
            .field("bearer_token", &"[redacted]")
            .finish()
    }
}

impl AuthPermissionChecker {
    pub(crate) fn from_env() -> Result<Option<Self>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Result<Option<Self>> {
        let origin =
            lookup("ORCHESTRATOR_AUTH_ADMIN_ORIGIN").filter(|value| !value.trim().is_empty());
        let token =
            lookup("ORCHESTRATOR_AUTH_ADMIN_TOKEN").filter(|value| !value.trim().is_empty());
        match (origin, token) {
            (None, None) => Ok(None),
            (Some(origin), Some(token)) => {
                let timeout = lookup(TIMEOUT_MS_ENV)
                    .map(|value| value.parse::<u64>())
                    .transpose()
                    .with_context(|| format!("{TIMEOUT_MS_ENV} must be an integer"))?
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_TIMEOUT);
                Self::new(&origin, token, timeout).map(Some)
            }
            _ => Err(anyhow!(
                "ORCHESTRATOR_AUTH_ADMIN_ORIGIN and ORCHESTRATOR_AUTH_ADMIN_TOKEN must both be configured for frontend permission checks"
            )),
        }
    }

    fn new(origin: &str, bearer_token: String, timeout: Duration) -> Result<Self> {
        let origin = normalize_origin(origin)?;
        if bearer_token.is_empty()
            || bearer_token != bearer_token.trim()
            || bearer_token.len() > 4096
            || !bearer_token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(anyhow!(
                "ORCHESTRATOR_AUTH_ADMIN_TOKEN is empty, padded, too long, or not header-safe"
            ));
        }
        if timeout.is_zero() || timeout > MAX_TIMEOUT {
            return Err(anyhow!(
                "{TIMEOUT_MS_ENV} must be between 1 and {} milliseconds",
                MAX_TIMEOUT.as_millis()
            ));
        }
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .build()
            .into();
        Ok(Self {
            url: origin,
            bearer_token,
            agent,
        })
    }

    /// Loads the effective system-scope permission set once for a batch. Any
    /// malformed principal, transport failure, upstream rejection or malformed
    /// acknowledgement yields no trusted set. The browser receives no
    /// diagnostic that could reveal the upstream endpoint or token.
    pub(crate) fn effective_permissions(&self, principal: &Principal) -> Option<BTreeSet<String>> {
        let user_id = auth_user_id(principal)?;
        self.load_user(user_id).ok()
    }

    fn load_user(&self, user_id: i64) -> Result<BTreeSet<String>> {
        let url = format!(
            "{}{EFFECTIVE_PATH_PREFIX}{user_id}/effective-permissions?scope_type=system",
            self.url
        );
        let response = self
            .agent
            .get(&url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", self.bearer_token))
            .call()
            .context("call Auth permission provider")?;
        if response.status().as_u16() != 200 {
            return Err(anyhow!("Auth effective permissions request was rejected"));
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
        if content_type != "application/json" {
            return Err(anyhow!(
                "Auth effective permissions returned invalid content type"
            ));
        }
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .context("read Auth permission response")?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(anyhow!("Auth effective permissions response is too large"));
        }
        let response: AuthEffectivePermissionsResponse =
            serde_json::from_slice(&body).context("decode Auth effective permissions")?;
        if response.code != 0
            || response.msg != "success"
            || response.data.user_id != user_id
            || response.data.scope_type != "system"
            || response.data.scope_id != 0
            || response
                .data
                .permissions
                .iter()
                .any(|permission| !valid_permission_key(permission))
        {
            return Err(anyhow!(
                "Auth effective permissions acknowledgement is invalid"
            ));
        }
        Ok(response.data.permissions.into_iter().collect())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthEffectivePermissionsResponse {
    code: i64,
    msg: String,
    data: AuthEffectivePermissionsData,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthEffectivePermissionsData {
    user_id: i64,
    scope_type: String,
    scope_id: i64,
    permissions: Vec<String>,
}

fn valid_permission_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.contains('.')
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn auth_user_id(principal: &Principal) -> Option<i64> {
    principal
        .id()
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
}

fn normalize_origin(raw: &str) -> Result<String> {
    if raw.is_empty() || raw != raw.trim() || raw.len() > 2048 {
        return Err(anyhow!("Auth origin is empty, padded, or too long"));
    }
    let url = url::Url::parse(raw).context("Auth origin is not a valid URL")?;
    if url.username() != ""
        || url.password().is_some()
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "Auth origin must contain only a scheme and authority without credentials"
        ));
    }
    match url.scheme() {
        "https" => {}
        "http" if loopback_host(url.host_str().unwrap_or_default()) => {}
        _ => return Err(anyhow!("Auth origin must use HTTPS; HTTP is loopback-only")),
    }
    Ok(raw.trim_end_matches('/').to_string())
}

fn loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}
