//! Deterministic service context contracts; no environment reads or I/O.

use crate::RuntimeError;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const MANAGED_SERVICE_CONTEXT_TARGET: &str = "/run/ojos/service";

pub const MANAGED_SERVICE_CONTEXT_FILE: &str = "/run/ojos/service/context.json";

pub const MANAGED_WORKLOAD_PUBLIC_KEY_FILE: &str = "/run/ojos/service/workload-public-key.pem";

const MAX_WORKLOAD_PUBLIC_KEY_PEM_BYTES: usize = 16 * 1024;

pub const MANAGED_SERVICE_CREDENTIAL_FILE: &str = "/run/ojos/service/token";

pub const MANAGED_SERVICE_GATEWAY_CA_FILE: &str = "/run/ojos/service/ca.pem";

pub const MANAGED_EVENT_CONTEXT_FILE: &str = "/run/ojos/service/events.json";

pub const MANAGED_EVENT_CONNECTION_FILE: &str = "/run/ojos/service/event-redis.url";

pub const MANAGED_EVENT_STREAM_V1: &str = "ojos:events:v1";

pub const MANAGED_RESOURCE_SECRET_ROOT: &str = "/run/ojos/resources";

pub const SERVICE_CONTRACT_GENERATION_LABEL: &str = "ojos.service_contract_generation";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedApiBinding {
    pub binding_id: String,
    pub api_id: String,
    pub timeout_ms: u64,
    #[serde(default)]
    pub context_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ManagedEventSubscription {
    pub event_type: String,
    pub consumer_group: String,
}

/// Credential-free event projection sent through the Job protocol. The Redis
/// URL/password never crosses the control-plane boundary; the Agent resolves
/// `connection_id` from its local protected connection configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedEventBinding {
    pub connection_id: String,
    pub stream: String,
    #[serde(default)]
    pub publish_types: Vec<String>,
    #[serde(default)]
    pub subscriptions: Vec<ManagedEventSubscription>,
    pub generation: u64,
}

/// Platform-owned verifier material for inbound workload JWTs. The public key
/// may cross the control-plane boundary, but the issuer private key never does.
/// The Agent writes the key to a dedicated file; it is not embedded in the
/// application-facing `context.json` document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedWorkloadVerifierSpec {
    pub public_key_pem: String,
    pub key_id: String,
    pub issuer: String,
    pub audience: String,
}

impl ManagedWorkloadVerifierSpec {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        validate_ed25519_spki_pem(&self.public_key_pem)?;
        validate_workload_token("workload verifier key_id", &self.key_id, 128, true)?;
        validate_workload_token("workload verifier issuer", &self.issuer, 512, false)?;
        validate_workload_token("workload verifier audience", &self.audience, 256, false)?;
        Ok(())
    }

    pub fn environment_sha256(&self) -> Result<String, RuntimeError> {
        self.validate()?;
        let bytes = serde_json::to_vec(&[
            self.key_id.as_str(),
            self.issuer.as_str(),
            self.audience.as_str(),
        ])
        .map_err(|error| {
            RuntimeError::InvalidRuntimeContext(format!(
                "cannot encode workload verifier environment: {error}"
            ))
        })?;
        Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

fn validate_workload_token(
    name: &str,
    value: &str,
    maximum: usize,
    strict_identifier: bool,
) -> Result<(), RuntimeError> {
    let valid_identifier = !strict_identifier
        || value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte));
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value.chars().any(char::is_whitespace)
        || !valid_identifier
    {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "{name} is outside its closed text bounds"
        )));
    }
    Ok(())
}

fn validate_ed25519_spki_pem(pem: &str) -> Result<(), RuntimeError> {
    let invalid = || {
        RuntimeError::InvalidRuntimeContext(
            "workload verifier public_key_pem must contain exactly one Ed25519 SubjectPublicKeyInfo PEM"
                .to_string(),
        )
    };
    if pem.is_empty()
        || pem.len() > MAX_WORKLOAD_PUBLIC_KEY_PEM_BYTES
        || !pem.is_ascii()
        || pem
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\r' | '\n'))
    {
        return Err(invalid());
    }
    let normalized = pem.replace("\r\n", "\n");
    let trimmed = normalized.trim_end_matches('\n');
    let lines = trimmed.lines().collect::<Vec<_>>();
    if lines.len() < 3
        || lines.first() != Some(&"-----BEGIN PUBLIC KEY-----")
        || lines.last() != Some(&"-----END PUBLIC KEY-----")
        || lines[1..lines.len() - 1].iter().any(|line| {
            line.is_empty()
                || line.len() > 76
                || !line
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        })
    {
        return Err(invalid());
    }
    let body = lines[1..lines.len() - 1].concat();
    let der = BASE64_STANDARD.decode(body).map_err(|_| invalid())?;
    const ED25519_SPKI_PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    if der.len() != ED25519_SPKI_PREFIX.len() + 32
        || !der.starts_with(ED25519_SPKI_PREFIX)
        || der[ED25519_SPKI_PREFIX.len()..]
            .iter()
            .all(|byte| *byte == 0)
    {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedServiceContextSpec {
    #[serde(default)]
    pub generation: u64,
    pub node_id: String,
    pub gateway_origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_ca_pem: Option<String>,
    pub bindings: BTreeMap<String, ManagedApiBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<ManagedEventBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_verifier: Option<ManagedWorkloadVerifierSpec>,
}

/// Credential-free control-plane projection used to rebuild an exact Agent
/// context CAS after the last binding was revoked. Tokens are never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedServiceContextProjection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<ManagedServiceContextSpec>,
    pub last_nonempty: ManagedServiceContextSpec,
    pub revoked: bool,
}

/// Agent-local, idempotent update of an already-running Deployment's mounted
/// Service Context. `context=None` revokes the local context during a topology
/// compensation/removal; a non-empty context is materialized atomically with a
/// freshly exchanged workload credential.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingContextApplyPayload {
    pub deployment_id: String,
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ManagedServiceContextSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_context: Option<ManagedServiceContextSpec>,
}

impl BindingContextApplyPayload {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        for (name, value) in [
            ("deployment_id", self.deployment_id.as_str()),
            ("service_id", self.service_id.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(RuntimeError::InvalidRuntimeContext(format!(
                    "{name} is empty or exceeds protocol bounds"
                )));
            }
        }
        if let Some(context) = &self.context {
            context.validate()?;
        }
        if let Some(context) = &self.previous_context {
            context.validate()?;
        }
        self.previous_context.as_ref().ok_or_else(|| {
            RuntimeError::InvalidRuntimeContext(
                "binding context apply requires the exact previous_context for CAS and compensation"
                    .to_string(),
            )
        })?;
        Ok(())
    }
}

impl ManagedServiceContextSpec {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.generation == 0 {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed service context generation must be positive".to_string(),
            ));
        }
        if self.node_id.trim().is_empty() {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed service context requires node_id".to_string(),
            ));
        }
        let origin = self.gateway_origin.as_str();
        let parsed = url::Url::parse(origin).map_err(|error| {
            RuntimeError::InvalidRuntimeContext(format!(
                "managed service gateway_origin is not a valid origin: {error}"
            ))
        })?;
        let has_authority_only = origin == origin.trim()
            && !origin.ends_with('/')
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.host_str().is_some();
        let loopback_http = parsed.scheme() == "http"
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"));
        if !has_authority_only || !(parsed.scheme() == "https" || loopback_http) {
            return Err(RuntimeError::InvalidRuntimeContext(
                "managed service gateway_origin must be an HTTPS scheme+authority without userinfo, path, query, or fragment, or loopback HTTP"
                    .to_string(),
            ));
        }
        for (name, binding) in &self.bindings {
            if name.trim().is_empty()
                || binding.binding_id.trim().is_empty()
                || binding.api_id.trim().is_empty()
                || binding.timeout_ms == 0
                || binding.context_generation != self.generation
                || binding.api_id.contains('/')
                || binding.api_id.chars().any(char::is_whitespace)
            {
                return Err(RuntimeError::InvalidRuntimeContext(format!(
                    "managed API binding {name:?} is incomplete or unsafe"
                )));
            }
        }
        if self
            .gateway_ca_pem
            .as_ref()
            .is_some_and(|pem| pem.is_empty() || pem.len() > 1024 * 1024)
        {
            return Err(RuntimeError::InvalidRuntimeContext(
                "gateway CA PEM must be non-empty and at most 1 MiB".to_string(),
            ));
        }
        if let Some(events) = &self.events {
            if events.generation != self.generation
                || events.connection_id.trim().is_empty()
                || events.stream.trim().is_empty()
                || events.stream.len() > 256
                || events.stream.chars().any(char::is_control)
                || (events.publish_types.is_empty() && events.subscriptions.is_empty())
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed event binding identity/generation is invalid".to_string(),
                ));
            }
            let mut publish_types = events.publish_types.clone();
            publish_types.sort();
            publish_types.dedup();
            if publish_types != events.publish_types
                || publish_types.iter().any(|event_type| {
                    event_type.trim().is_empty()
                        || event_type.len() > 256
                        || event_type.chars().any(char::is_whitespace)
                })
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed event publish types must be sorted unique identifiers".to_string(),
                ));
            }
            let mut subscriptions = events.subscriptions.clone();
            subscriptions.sort();
            subscriptions.dedup();
            if subscriptions != events.subscriptions
                || subscriptions.iter().any(|subscription| {
                    subscription.event_type.trim().is_empty()
                        || subscription.consumer_group.trim().is_empty()
                        || subscription.event_type.len() > 256
                        || subscription.consumer_group.len() > 256
                        || subscription.event_type.chars().any(char::is_whitespace)
                        || subscription.consumer_group.chars().any(char::is_whitespace)
                })
            {
                return Err(RuntimeError::InvalidRuntimeContext(
                    "managed event subscriptions must be sorted unique identifiers/groups"
                        .to_string(),
                ));
            }
        }
        if let Some(verifier) = &self.workload_verifier {
            verifier.validate()?;
        }
        Ok(())
    }
}
