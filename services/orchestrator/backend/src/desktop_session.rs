use getrandom::fill;
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;

pub(crate) const DESKTOP_BOOTSTRAP_HEADER: &str = "x-ojos-desktop-bootstrap";
pub(crate) const DESKTOP_CSRF_HEADER: &str = "x-csrf-token";
pub(crate) const DESKTOP_COOKIE_NAME: &str = "ojos_session";
const BOOTSTRAP_TTL: Duration = Duration::from_secs(60);
const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum DesktopSessionError {
    #[error("desktop bootstrap secret is invalid or expired")]
    InvalidBootstrap,
    #[error("desktop bootstrap secret was already consumed")]
    BootstrapConsumed,
    #[error("desktop session is invalid")]
    InvalidSession,
    #[error("desktop CSRF token is missing or invalid")]
    InvalidCsrf,
    #[error("failed to generate desktop session entropy")]
    Entropy,
    #[error("desktop session lock poisoned")]
    Poisoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSession {
    pub session_id: String,
    pub csrf_token: String,
}

#[derive(Debug)]
struct State {
    bootstrap_hash: [u8; 32],
    bootstrap_expires_at: Instant,
    bootstrap_consumed: bool,
    session_hash: Option<[u8; 32]>,
    csrf_hash: Option<[u8; 32]>,
    session_expires_at: Option<Instant>,
}

#[derive(Debug)]
pub(crate) struct DesktopSessionManager {
    state: Mutex<State>,
    session_ttl: Duration,
}

impl DesktopSessionManager {
    pub fn new(bootstrap_secret: &str) -> Self {
        Self::with_ttls(bootstrap_secret, BOOTSTRAP_TTL, SESSION_TTL)
    }

    fn with_ttls(bootstrap_secret: &str, bootstrap_ttl: Duration, session_ttl: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                bootstrap_hash: hash(bootstrap_secret),
                bootstrap_expires_at: Instant::now() + bootstrap_ttl,
                bootstrap_consumed: false,
                session_hash: None,
                csrf_hash: None,
                session_expires_at: None,
            }),
            session_ttl,
        }
    }

    pub fn exchange(&self, bootstrap_secret: &str) -> Result<DesktopSession, DesktopSessionError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| DesktopSessionError::Poisoned)?;
        if state.bootstrap_consumed {
            return Err(DesktopSessionError::BootstrapConsumed);
        }
        if Instant::now() > state.bootstrap_expires_at
            || !constant_time_eq(&state.bootstrap_hash, &hash(bootstrap_secret))
        {
            return Err(DesktopSessionError::InvalidBootstrap);
        }
        let session_id = random_hex()?;
        let csrf_token = random_hex()?;
        state.bootstrap_consumed = true;
        state.bootstrap_hash.fill(0);
        state.session_hash = Some(hash(&session_id));
        state.csrf_hash = Some(hash(&csrf_token));
        state.session_expires_at = Some(Instant::now() + self.session_ttl);
        Ok(DesktopSession {
            session_id,
            csrf_token,
        })
    }

    pub fn authorize(
        &self,
        cookie_header: Option<&str>,
        csrf_header: Option<&str>,
        mutation: bool,
    ) -> Result<bool, DesktopSessionError> {
        let Some(session_id) = cookie_value(cookie_header, DESKTOP_COOKIE_NAME) else {
            return Ok(false);
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| DesktopSessionError::Poisoned)?;
        if state
            .session_expires_at
            .is_some_and(|expires_at| expires_at <= Instant::now())
        {
            state.session_hash = None;
            state.csrf_hash = None;
            state.session_expires_at = None;
            return Err(DesktopSessionError::InvalidSession);
        }
        let Some(expected_session) = state.session_hash else {
            return Err(DesktopSessionError::InvalidSession);
        };
        if !constant_time_eq(&expected_session, &hash(session_id)) {
            return Err(DesktopSessionError::InvalidSession);
        }
        if mutation {
            let actual = csrf_header.ok_or(DesktopSessionError::InvalidCsrf)?;
            let expected = state.csrf_hash.ok_or(DesktopSessionError::InvalidCsrf)?;
            if !constant_time_eq(&expected, &hash(actual)) {
                return Err(DesktopSessionError::InvalidCsrf);
            }
        }
        Ok(true)
    }
}

pub(crate) fn session_cookie(session_id: &str) -> String {
    format!("{DESKTOP_COOKIE_NAME}={session_id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=28800")
}

fn cookie_value<'a>(header: Option<&'a str>, name: &str) -> Option<&'a str> {
    header?.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then_some(value)
    })
}

fn random_hex() -> Result<String, DesktopSessionError> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes).map_err(|_| DesktopSessionError::Entropy)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn hash(value: &str) -> [u8; 32] {
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
