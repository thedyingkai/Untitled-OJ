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
            "{}{EFFECTIVE_PATH_PREFIX}{user_id}?scope_type=system",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Principal, PrincipalSource};
    use orchestrator_legacy::V1Role;
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::thread;

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "request ended before headers");
            bytes.extend_from_slice(&chunk[..read]);
            let Some(header_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    fn mock_response(
        status: &'static str,
        content_type: &'static str,
        body: &'static str,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            assert!(request.starts_with("GET /auth/admin/users/42?scope_type=system HTTP/1.1"));
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("authorization: bearer admin-control-plane"));
            assert!(!lower.contains("x-ojos-caller-service"));
            assert!(!lower.contains("x-ojos-api-id"));
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            stream.flush().unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        (format!("http://{address}"), server)
    }

    fn checker(origin: &str) -> AuthPermissionChecker {
        AuthPermissionChecker::new(
            origin,
            "admin-control-plane".to_string(),
            Duration::from_secs(2),
        )
        .unwrap()
    }

    fn principal(id: &str) -> Principal {
        Principal::verified(id, V1Role::Admin, PrincipalSource::Oidc)
    }

    #[test]
    fn exact_auth_acknowledgement_returns_the_authenticated_users_effective_set() {
        let (origin, server) = mock_response(
            "200 OK",
            "application/json",
            r#"{"code":0,"msg":"success","data":{"user_id":42,"scope_type":"system","scope_id":0,"permissions":["contest-service.contest.manage"]}}"#,
        );
        let permissions = checker(&origin)
            .effective_permissions(&principal("42"))
            .unwrap();
        assert!(permissions.contains("contest-service.contest.manage"));
        server.join().unwrap();
    }

    #[test]
    fn unknown_or_malformed_auth_results_fail_closed() {
        for body in [
            r#"{"code":0,"msg":"success","data":{"user_id":43,"scope_type":"system","scope_id":0,"permissions":[]}}"#,
            r#"{"code":0,"msg":"success","data":{"user_id":42,"scope_type":"system","scope_id":0,"permissions":[]},"principal":42}"#,
            r#"{"code":0,"msg":"not-success","data":{"user_id":42,"scope_type":"system","scope_id":0,"permissions":[]}}"#,
        ] {
            let (origin, server) = mock_response("200 OK", "application/json", body);
            assert!(
                checker(&origin)
                    .effective_permissions(&principal("42"))
                    .is_none()
            );
            server.join().unwrap();
        }
        assert!(
            AuthPermissionChecker::new(
                "http://127.0.0.1:9",
                "admin-control-plane".to_string(),
                Duration::from_millis(50)
            )
            .unwrap()
            .effective_permissions(&principal("42"))
            .is_none()
        );
        // Orchestrator Desktop/OIDC identities that cannot be mapped exactly to
        // Auth's positive integer user_id never trigger an upstream request.
        assert!(
            checker("http://127.0.0.1:9")
                .effective_permissions(&principal("username-or-subject"))
                .is_none()
        );
    }

    #[test]
    fn configuration_is_paired_and_http_is_loopback_only() {
        let values = BTreeMap::from([(
            "ORCHESTRATOR_AUTH_ADMIN_ORIGIN".to_string(),
            "https://auth.example".to_string(),
        )]);
        assert!(AuthPermissionChecker::from_lookup(|name| values.get(name).cloned()).is_err());
        assert!(
            AuthPermissionChecker::new(
                "http://auth.internal",
                "token".to_string(),
                DEFAULT_TIMEOUT
            )
            .is_err()
        );
        assert!(
            AuthPermissionChecker::new(
                "https://auth.example/path",
                "token".to_string(),
                DEFAULT_TIMEOUT
            )
            .is_err()
        );
    }
}
