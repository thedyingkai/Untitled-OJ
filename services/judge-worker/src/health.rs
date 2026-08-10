use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug)]
pub struct HealthState {
    preflight_ok: AtomicBool,
    registered: AtomicBool,
    last_heartbeat_ms: AtomicI64,
    heartbeat_interval_ms: AtomicI64,
}

#[derive(Debug, Serialize)]
struct HealthDocument {
    status: &'static str,
    checks: HealthChecks,
}

#[derive(Debug, Serialize)]
struct HealthChecks {
    preflight: bool,
    registered: bool,
    heartbeat_fresh: bool,
    last_heartbeat_ms: i64,
}

impl Default for HealthState {
    fn default() -> Self {
        Self {
            preflight_ok: AtomicBool::new(false),
            registered: AtomicBool::new(false),
            last_heartbeat_ms: AtomicI64::new(0),
            heartbeat_interval_ms: AtomicI64::new(10_000),
        }
    }
}

impl HealthState {
    pub fn mark_preflight_ok(&self, heartbeat_interval: Duration) {
        self.heartbeat_interval_ms.store(
            heartbeat_interval
                .as_millis()
                .try_into()
                .unwrap_or(i64::MAX),
            Ordering::Release,
        );
        self.preflight_ok.store(true, Ordering::Release);
    }

    pub fn mark_registered(&self) {
        self.registered.store(true, Ordering::Release);
        self.mark_heartbeat();
    }

    pub fn mark_disconnected(&self) {
        self.registered.store(false, Ordering::Release);
    }

    pub fn mark_heartbeat(&self) {
        self.last_heartbeat_ms.store(now_ms(), Ordering::Release);
    }

    fn checks(&self) -> HealthChecks {
        let last_heartbeat_ms = self.last_heartbeat_ms.load(Ordering::Acquire);
        let heartbeat_interval_ms = self.heartbeat_interval_ms.load(Ordering::Acquire);
        let max_age = heartbeat_interval_ms
            .saturating_mul(2)
            .saturating_add(5_000);
        let heartbeat_fresh = last_heartbeat_ms > 0
            && now_ms().saturating_sub(last_heartbeat_ms) <= max_age.max(30_000);
        HealthChecks {
            preflight: self.preflight_ok.load(Ordering::Acquire),
            registered: self.registered.load(Ordering::Acquire),
            heartbeat_fresh,
            last_heartbeat_ms,
        }
    }

    pub fn ready(&self) -> bool {
        let checks = self.checks();
        checks.preflight && checks.registered && checks.heartbeat_fresh
    }

    fn document(&self, live: bool) -> HealthDocument {
        let checks = self.checks();
        let ready = checks.preflight && checks.registered && checks.heartbeat_fresh;
        HealthDocument {
            status: if live || ready { "ok" } else { "not_ready" },
            checks,
        }
    }
}

pub async fn serve(state: Arc<HealthState>) -> Result<()> {
    let address =
        std::env::var("OJOS_HEALTH_LISTEN").unwrap_or_else(|_| "0.0.0.0:9101".to_string());
    let listener = TcpListener::bind(&address)
        .await
        .with_context(|| format!("bind judge-worker health endpoint on {address}"))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, state).await {
                tracing::warn!(%error, "judge-worker health request failed");
            }
        });
    }
}

async fn serve_connection(mut stream: TcpStream, state: Arc<HealthState>) -> Result<()> {
    let mut buffer = [0_u8; 4096];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
        .await
        .context("health request timed out")??;
    let request = std::str::from_utf8(&buffer[..read]).unwrap_or_default();
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, document) = match path {
        "/healthz/live" => ("200 OK", state.document(true)),
        "/health" | "/healthz/ready" if state.ready() => ("200 OK", state.document(false)),
        "/health" | "/healthz/ready" => ("503 Service Unavailable", state.document(false)),
        _ => (
            "404 Not Found",
            HealthDocument {
                status: "not_ready",
                checks: state.checks(),
            },
        ),
    };
    let body = serde_json::to_vec(&document)?;
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    Ok(())
}

pub async fn check_ready() -> Result<()> {
    let url = std::env::var("OJOS_HEALTHCHECK_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9101/healthz/ready".to_string());
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("request readiness endpoint failed: {url}"))?;
    if !response.status().is_success() {
        return Err(anyhow!("readiness endpoint returned {}", response.status()));
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_preflight_registration_and_fresh_heartbeat() {
        let state = HealthState::default();
        assert!(!state.ready());
        state.mark_preflight_ok(Duration::from_secs(10));
        assert!(!state.ready());
        state.mark_registered();
        assert!(state.ready());
        state.mark_disconnected();
        assert!(!state.ready());
    }
}
