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
