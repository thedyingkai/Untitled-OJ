use crate::{Endpoint, Link, OrchestratorError, Result, validate_endpoint_id};
use serde::{Deserialize, Serialize};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointHealthResult {
    pub endpoint: String,
    pub health: String,
    pub reachable: bool,
    pub latency_ms: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkHealthResult {
    pub source_endpoint: String,
    pub target_endpoint: String,
    pub health: String,
    pub latency_ms: Option<u32>,
    pub message: String,
}

pub trait EndpointProbe {
    fn probe(&self, endpoint: &Endpoint) -> Result<EndpointHealthResult>;
}

#[derive(Debug, Clone)]
pub struct TcpEndpointProbe {
    timeout: Duration,
}

#[derive(Debug, Default, Clone)]
pub struct StaticEndpointProbe;

impl TcpEndpointProbe {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl EndpointProbe for TcpEndpointProbe {
    fn probe(&self, endpoint: &Endpoint) -> Result<EndpointHealthResult> {
        validate_endpoint_id(&endpoint.endpoint)?;
        let start = Instant::now();
        let mut addrs = endpoint.endpoint.to_socket_addrs().map_err(|err| {
            OrchestratorError::Dependency(format!(
                "endpoint {} cannot resolve: {err}",
                endpoint.endpoint
            ))
        })?;
        let Some(addr) = addrs.next() else {
            return Ok(EndpointHealthResult {
                endpoint: endpoint.endpoint.clone(),
                health: "unreachable".to_string(),
                reachable: false,
                latency_ms: None,
                message: "endpoint has no socket address".to_string(),
            });
        };
        match TcpStream::connect_timeout(&addr, self.timeout) {
            Ok(_) => Ok(EndpointHealthResult {
                endpoint: endpoint.endpoint.clone(),
                health: "healthy".to_string(),
                reachable: true,
                latency_ms: Some(start.elapsed().as_millis().min(u128::from(u32::MAX)) as u32),
                message: format!("{} tcp reachable", endpoint.protocol),
            }),
            Err(err) => Ok(EndpointHealthResult {
                endpoint: endpoint.endpoint.clone(),
                health: "unreachable".to_string(),
                reachable: false,
                latency_ms: None,
                message: err.to_string(),
            }),
        }
    }
}

impl EndpointProbe for StaticEndpointProbe {
    fn probe(&self, endpoint: &Endpoint) -> Result<EndpointHealthResult> {
        validate_endpoint_id(&endpoint.endpoint)?;
        let health = if endpoint.reachable {
            normalize_health(&endpoint.health)
        } else {
            "unreachable".to_string()
        };
        Ok(EndpointHealthResult {
            endpoint: endpoint.endpoint.clone(),
            health,
            reachable: endpoint.reachable,
            latency_ms: None,
            message: "static endpoint state checked".to_string(),
        })
    }
}

pub fn check_endpoint_health_with_probe<P: EndpointProbe>(
    endpoint: &Endpoint,
    probe: &P,
) -> Result<EndpointHealthResult> {
    match endpoint.protocol.as_str() {
        "http" | "https" | "tcp" | "postgres" | "redis" => probe.probe(endpoint),
        value => Ok(EndpointHealthResult {
            endpoint: endpoint.endpoint.clone(),
            health: "blocked".to_string(),
            reachable: false,
            latency_ms: None,
            message: format!("unsupported endpoint protocol {value}"),
        }),
    }
}

pub fn check_link_health(
    link: &Link,
    endpoints: &[Endpoint],
    target_health: &EndpointHealthResult,
) -> Result<LinkHealthResult> {
    validate_endpoint_id(&link.source_endpoint)?;
    validate_endpoint_id(&link.target_endpoint)?;
    let source = endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint == link.source_endpoint);
    let target = endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint == link.target_endpoint);
    if source.is_none() {
        return Ok(link_result(
            link,
            "blocked",
            None,
            "source endpoint is missing",
        ));
    }
    let Some(target) = target else {
        return Ok(link_result(
            link,
            "blocked",
            None,
            "target endpoint is missing",
        ));
    };
    if !target_health.reachable {
        return Ok(link_result(
            link,
            "unreachable",
            target_health.latency_ms,
            "target endpoint is unreachable",
        ));
    }
    if protocol_family(&link.protocol) != protocol_family(&target.protocol) {
        return Ok(link_result(
            link,
            "degraded",
            target_health.latency_ms,
            "link protocol does not match target endpoint",
        ));
    }
    if link.auth_mode.trim().is_empty() || link.scope.trim().is_empty() {
        return Ok(link_result(
            link,
            "degraded",
            target_health.latency_ms,
            "link auth_mode or scope is incomplete",
        ));
    }
    Ok(link_result(
        link,
        "healthy",
        target_health.latency_ms,
        "link policy and reachability checked",
    ))
}

fn link_result(
    link: &Link,
    health: &str,
    latency_ms: Option<u32>,
    message: &str,
) -> LinkHealthResult {
    LinkHealthResult {
        source_endpoint: link.source_endpoint.clone(),
        target_endpoint: link.target_endpoint.clone(),
        health: health.to_string(),
        latency_ms,
        message: message.to_string(),
    }
}

fn normalize_health(value: &str) -> String {
    match value {
        "healthy" | "degraded" | "blocked" | "unreachable" | "unknown" => value.to_string(),
        "ok" => "healthy".to_string(),
        "" => "unknown".to_string(),
        _ => "degraded".to_string(),
    }
}

fn protocol_family(value: &str) -> &str {
    match value {
        "http" | "https" => "http",
        "postgres" => "postgres",
        "redis" => "redis",
        _ => "tcp",
    }
}
