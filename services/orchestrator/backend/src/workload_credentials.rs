use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const ISSUE_PATH: &str = "/auth/internal/workload-tokens:issue";
const MAX_RESPONSE_BYTES: usize = 32 * 1024;
pub(crate) const WORKLOAD_TOKEN_TTL_SECONDS: u64 = 15 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkloadTokenRequest {
    pub(crate) deployment_id: String,
    pub(crate) service_id: String,
    pub(crate) node_id: String,
    pub(crate) credential_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssuedWorkloadToken {
    pub(crate) access_token: String,
    pub(crate) expires_at_ms: i64,
    pub(crate) expires_in: u64,
}

pub(crate) trait WorkloadTokenIssuer: Send + Sync {
    fn issue(&self, request: &WorkloadTokenRequest) -> Result<IssuedWorkloadToken>;
}

#[derive(Clone)]
pub(crate) struct HttpWorkloadTokenIssuer {
    url: String,
    internal_token: String,
    agent: ureq::Agent,
}

impl HttpWorkloadTokenIssuer {
    pub(crate) fn from_env(production: bool) -> Result<Option<Self>> {
        let allow_compose_bootstrap_http =
            std::env::var("ORCHESTRATOR_ALLOW_COMPOSE_BOOTSTRAP_HTTP")
                .ok()
                .is_some_and(|value| {
                    matches!(value.trim(), "1") || value.trim().eq_ignore_ascii_case("true")
                });
        Self::from_lookup(production, allow_compose_bootstrap_http, |name| {
            std::env::var(name).ok()
        })
    }

    fn from_lookup(
        production: bool,
        allow_compose_bootstrap_http: bool,
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Option<Self>> {
        // Deliberately do not inspect AUTH admin or generic orchestrator
        // tokens. Workload issuance has a dedicated least-privilege
        // control-plane credential and must fail closed when only half of the
        // pair is configured.
        let origin = lookup("ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let token = lookup("ORCHESTRATOR_AUTH_WORKLOAD_TOKEN")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        match (origin, token) {
            (None, None) if !production => Ok(None),
            (None, None) => Err(anyhow!(
                "production requires ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN and ORCHESTRATOR_AUTH_WORKLOAD_TOKEN"
            )),
            (Some(origin), Some(token)) => {
                validate_issuer_origin(&origin, production, allow_compose_bootstrap_http)?;
                Self::new(&origin, token).map(Some)
            }
            _ => Err(anyhow!(
                "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN and ORCHESTRATOR_AUTH_WORKLOAD_TOKEN must be configured together"
            )),
        }
    }

    pub(crate) fn new(origin: &str, internal_token: String) -> Result<Self> {
        let origin = origin.trim().trim_end_matches('/');
        if origin.is_empty() || internal_token.trim().is_empty() {
            return Err(anyhow!(
                "workload issuer origin and internal token are required"
            ));
        }
        let url = format!("{origin}{ISSUE_PATH}");
        let uri = url
            .parse::<ureq::http::Uri>()
            .context("parse Auth workload issuer URL")?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
            return Err(anyhow!("Auth workload issuer URL must be absolute HTTP(S)"));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        Ok(Self {
            url,
            internal_token,
            agent,
        })
    }
}

fn validate_issuer_origin(
    origin: &str,
    production: bool,
    allow_compose_bootstrap_http: bool,
) -> Result<()> {
    let parsed = url::Url::parse(origin).context("parse Auth workload issuer origin")?;
    if parsed.scheme() == "https" {
        return Ok(());
    }
    if parsed.scheme() != "http" {
        return Err(anyhow!("Auth workload issuer origin must use HTTPS"));
    }
    let host = parsed.host_str().unwrap_or_default();
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    let compose_bootstrap = allow_compose_bootstrap_http && host == "auth-service";
    if production && !loopback && !compose_bootstrap {
        return Err(anyhow!(
            "production Auth workload issuer origin must use HTTPS; plaintext is limited to loopback or the explicitly enabled auth-service Compose bootstrap network"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::net::{Shutdown, TcpListener, TcpStream};

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "issuer request ended before its JSON body");
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = std::str::from_utf8(&request[..header_end])
                .expect("issuer request headers must be valid UTF-8");
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                })
                .expect("issuer request Content-Length");
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(request).expect("issuer request must be valid UTF-8")
    }

    #[test]
    fn admin_and_generic_tokens_are_never_workload_issuer_fallbacks() {
        let values = BTreeMap::from([
            (
                "ORCHESTRATOR_AUTH_ADMIN_ORIGIN".to_string(),
                "https://auth-admin.example".to_string(),
            ),
            (
                "ORCHESTRATOR_AUTH_ADMIN_TOKEN".to_string(),
                "admin-secret".to_string(),
            ),
            (
                "ORCHESTRATOR_INTERNAL_TOKEN".to_string(),
                "generic-secret".to_string(),
            ),
        ]);
        assert!(
            HttpWorkloadTokenIssuer::from_lookup(false, false, |name| values.get(name).cloned())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn workload_issuer_requires_its_dedicated_origin_and_token_as_a_pair() {
        let origin_only = BTreeMap::from([(
            "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN".to_string(),
            "https://auth.example".to_string(),
        )]);
        assert!(
            HttpWorkloadTokenIssuer::from_lookup(false, false, |name| origin_only
                .get(name)
                .cloned())
            .is_err()
        );

        let dedicated = BTreeMap::from([
            (
                "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN".to_string(),
                "https://auth.example".to_string(),
            ),
            (
                "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN".to_string(),
                "workload-only".to_string(),
            ),
        ]);
        assert!(
            HttpWorkloadTokenIssuer::from_lookup(false, false, |name| dedicated.get(name).cloned())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn production_requires_secure_and_complete_workload_issuer() {
        assert!(HttpWorkloadTokenIssuer::from_lookup(true, false, |_| None).is_err());
        let insecure = BTreeMap::from([
            (
                "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN".to_string(),
                "http://auth-service:8081".to_string(),
            ),
            (
                "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN".to_string(),
                "dedicated-token".to_string(),
            ),
        ]);
        assert!(
            HttpWorkloadTokenIssuer::from_lookup(true, false, |name| insecure.get(name).cloned())
                .is_err()
        );
        assert!(
            HttpWorkloadTokenIssuer::from_lookup(true, true, |name| insecure.get(name).cloned())
                .unwrap()
                .is_some()
        );
        let arbitrary = BTreeMap::from([
            (
                "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN".to_string(),
                "http://auth-other:8081".to_string(),
            ),
            (
                "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN".to_string(),
                "dedicated-token".to_string(),
            ),
        ]);
        assert!(
            HttpWorkloadTokenIssuer::from_lookup(true, true, |name| arbitrary.get(name).cloned())
                .is_err()
        );
    }

    #[test]
    fn auth_issuer_response_is_a_distinct_closed_internal_contract() {
        let exact = serde_json::json!({
            "access_token": "secret-token",
            "token_type": "Bearer",
            "expires_at": "2030-01-01T00:00:00Z",
            "expires_in": 900,
        });
        assert!(serde_json::from_value::<AuthIssueResponse>(exact.clone()).is_ok());

        let mut decorated = exact.clone();
        decorated["status"] = serde_json::json!("ok");
        assert!(serde_json::from_value::<AuthIssueResponse>(decorated).is_err());
        assert!(
            serde_json::from_value::<AuthIssueResponse>(serde_json::json!({
                "data": exact,
                "meta": {"request_id": "req-1", "api_version": "v1"}
            }))
            .is_err()
        );
    }

    #[test]
    fn auth_rfc3339_expiry_is_converted_to_unix_milliseconds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /auth/internal/workload-tokens:issue "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer workload-control-plane")
            );
            let body = r#"{"access_token":"signed-token","token_type":"Bearer","expires_at":"1970-01-01T00:00:01.234Z","expires_in":900}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            stream.flush().unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        let issuer = HttpWorkloadTokenIssuer::new(
            &format!("http://{address}"),
            "workload-control-plane".to_string(),
        )
        .unwrap();
        let issued = issuer
            .issue(&WorkloadTokenRequest {
                deployment_id: "deployment-b".to_string(),
                service_id: "judge-worker".to_string(),
                node_id: "node-b".to_string(),
                credential_generation: 3,
            })
            .unwrap();
        server.join().unwrap();
        assert_eq!(issued.expires_at_ms, 1_234);
        assert_eq!(issued.expires_in, 900);
        assert_eq!(issued.access_token, "signed-token");
    }

    #[test]
    fn auth_issuer_rejects_non_utf8_json_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /auth/internal/workload-tokens:issue "));
            let body = b"{\"access_token\":\"\xff\",\"token_type\":\"Bearer\",\"expires_at\":\"1970-01-01T00:00:01.234Z\",\"expires_in\":900}";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            )
            .unwrap();
            stream.write_all(body).unwrap();
            stream.flush().unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        let issuer = HttpWorkloadTokenIssuer::new(
            &format!("http://{address}"),
            "workload-control-plane".to_string(),
        )
        .unwrap();
        let error = issuer
            .issue(&WorkloadTokenRequest {
                deployment_id: "deployment-b".to_string(),
                service_id: "judge-worker".to_string(),
                node_id: "node-b".to_string(),
                credential_generation: 3,
            })
            .unwrap_err();
        server.join().unwrap();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("Auth workload issuer response is not valid UTF-8"),
            "unexpected issuer diagnostic: {diagnostic}"
        );
    }
}

#[derive(Serialize)]
struct AuthIssueRequest<'a> {
    deployment_id: &'a str,
    service_id: &'a str,
    node_id: &'a str,
    credential_generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthIssueResponse {
    access_token: String,
    token_type: String,
    expires_at: String,
    expires_in: u64,
}

impl WorkloadTokenIssuer for HttpWorkloadTokenIssuer {
    fn issue(&self, request: &WorkloadTokenRequest) -> Result<IssuedWorkloadToken> {
        let payload = serde_json::to_vec(&AuthIssueRequest {
            deployment_id: &request.deployment_id,
            service_id: &request.service_id,
            node_id: &request.node_id,
            credential_generation: request.credential_generation,
        })?;
        let response = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", self.internal_token))
            .send(payload)
            .context("call Auth workload token issuer")?;
        let status = response.status().as_u16();
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
        if content_type != "application/json" && content_type != "application/problem+json" {
            return Err(anyhow!(
                "Auth workload issuer returned unsupported Content-Type"
            ));
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .context("read Auth workload issuer response")?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(anyhow!("Auth workload issuer response is too large"));
        }
        if !(200..=299).contains(&status) {
            return Err(anyhow!(
                "Auth workload issuer rejected request with HTTP {status}"
            ));
        }
        let body = std::str::from_utf8(&bytes)
            .context("Auth workload issuer response is not valid UTF-8")?;
        let issued: AuthIssueResponse =
            serde_json::from_str(body).context("decode Auth workload issuer response")?;
        if issued.token_type != "Bearer"
            || issued.expires_in != WORKLOAD_TOKEN_TTL_SECONDS
            || issued.access_token.is_empty()
            || issued.access_token.len() > 16 * 1024
            || issued.access_token.chars().any(char::is_whitespace)
        {
            return Err(anyhow!(
                "Auth workload issuer returned an invalid 15-minute credential"
            ));
        }
        let expires_at = OffsetDateTime::parse(&issued.expires_at, &Rfc3339)
            .context("parse Auth expires_at as RFC3339")?;
        let expires_at_ms = expires_at
            .unix_timestamp_nanos()
            .div_euclid(1_000_000)
            .try_into()
            .map_err(|_| anyhow!("Auth expires_at is outside the supported Unix-ms range"))?;
        Ok(IssuedWorkloadToken {
            access_token: issued.access_token,
            expires_at_ms,
            expires_in: issued.expires_in,
        })
    }
}
