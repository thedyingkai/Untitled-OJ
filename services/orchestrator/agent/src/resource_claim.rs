//! Agent-local resource claim execution for the first managed resource type.
//!
//! PostgreSQL administrator credentials are configuration of the concrete executor
//! and never enter these types. Generated database credentials live only in
//! [`SecretMaterial`] and a node-local [`ResourceSecretStore`]. Every serializable
//! record contains references and digests, never credential bytes or a DSN.

use fs2::FileExt;
use orchestrator_runtime::WorkloadFileOwnership;
use postgres::{
    Client as PostgreSqlClient, Config as PostgreSqlConfig,
    config::{Host as PostgreSqlHost, SslMode as PostgreSqlSslMode},
};
use postgres_protocol::password::scram_sha_256;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use rustls_tokio_postgres::{MakeRustlsConnect, config_from_ca_cert, config_platform_verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::IpAddr,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;

pub const RESOURCE_CLAIM_SCHEMA_VERSION: &str = "ojos.dev/resource-claim/v1";
pub const RESOURCE_TYPE_POSTGRESQL_DATABASE: &str = "postgresql.database/v1";
pub const OUTPUT_SECRET_MODE: u32 = 0o600;
pub const GENERATED_PASSWORD_BYTES: usize = 32;

const POSTGRES_COMMAND_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS resource_claim_commands (
    idempotency_key TEXT PRIMARY KEY,
    request_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('STARTED', 'COMPLETED')),
    evidence_json TEXT,
    updated_at_ms INTEGER NOT NULL
);
"#;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ResourceClaimError {
    #[error("unsupported resource claim schema version {actual}")]
    UnsupportedSchemaVersion { actual: String },
    #[error("{field} has invalid identifier {value}")]
    InvalidIdentifier { field: &'static str, value: String },
    #[error("{field} has invalid canonical sha256 digest {value}")]
    InvalidDigest { field: &'static str, value: String },
    #[error("resource claim generation must be greater than zero")]
    InvalidGeneration,
    #[error("resource claim digest mismatch: expected {expected}, found {actual}")]
    ClaimDigestMismatch { expected: String, actual: String },
    #[error("unsupported resource type {0}")]
    UnsupportedResourceType(String),
    #[error("resource lifecycle must be RETAIN")]
    UnsupportedLifecycle,
    #[error("invalid status transition {from:?} -> {to:?}")]
    InvalidStatusTransition {
        from: ResourceClaimStatusV1,
        to: ResourceClaimStatusV1,
    },
    #[error("action {action:?} is not allowed while claim is {status:?}")]
    ActionNotAllowed {
        action: ResourceClaimActionKindV1,
        status: ResourceClaimStatusV1,
    },
    #[error("provider descriptor is invalid: {0}")]
    InvalidProvider(String),
    #[error("purge confirmation mismatch; expected exact confirmation {expected}")]
    PurgeConfirmationMismatch { expected: String },
    #[error("purge audit intent is invalid: {0}")]
    InvalidPurgeAuditIntent(String),
    #[error("password generator failed: {0}")]
    PasswordGeneration(String),
    #[error("secret store failed: {0}")]
    SecretStore(String),
    #[error("provider executor failed: {0}")]
    Provider(String),
    #[error("resource claim execution ended without a proven outcome")]
    ExecutionOutcomeUnknown,
    #[error("provider returned evidence that does not match the command: {0}")]
    InvalidEvidence(String),
    #[error("secret write evidence is invalid: {0}")]
    InvalidSecretEvidence(String),
}

pub type Result<T> = std::result::Result<T, ResourceClaimError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResourceClaimStatusV1 {
    Requested,
    Provisioning,
    Ready,
    Releasing,
    Retained,
    Purging,
    Deleted,
    Failed,
    NeedsAttention,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ResourceLifecycleV1 {
    #[default]
    Retain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceClaimIdentityV1 {
    pub claim_id: String,
    /// Stable service-instance/install owner. A release deployment is only a
    /// binding and never participates in PostgreSQL naming or claim identity.
    #[serde(alias = "deploymentId")]
    pub owner_instance_id: String,
    pub service_id: String,
    pub resource_name: String,
    pub resource_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceClaimV1 {
    pub schema_version: String,
    pub identity: ResourceClaimIdentityV1,
    pub claim_digest: String,
    pub generation: u64,
    pub lifecycle: ResourceLifecycleV1,
    pub provider_id: String,
    pub status: ResourceClaimStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_secret: Option<ResourceOutputSecretV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<PostgreSqlEvidenceV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ResourceClaimFailureV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_audit_intent_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceOutputSecretV1 {
    pub reference: String,
    pub content_digest: String,
    pub mode: u32,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceClaimFailureV1 {
    pub code: ResourceClaimFailureCodeV1,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResourceClaimFailureCodeV1 {
    ProviderRejected,
    ProviderUnavailable,
    ProviderFactUnknown,
    ProviderEvidenceMismatch,
    CredentialMaterialUnavailable,
    SecretMaterializationFailed,
    IdempotencyConflict,
}

impl ResourceClaimV1 {
    pub fn requested(
        identity: ResourceClaimIdentityV1,
        generation: u64,
        provider_id: impl Into<String>,
    ) -> Result<Self> {
        if generation == 0 {
            return Err(ResourceClaimError::InvalidGeneration);
        }
        validate_identity(&identity)?;
        let claim_digest = resource_claim_digest(&identity)?;
        let claim = Self {
            schema_version: RESOURCE_CLAIM_SCHEMA_VERSION.to_string(),
            identity,
            claim_digest,
            generation,
            lifecycle: ResourceLifecycleV1::Retain,
            provider_id: provider_id.into(),
            status: ResourceClaimStatusV1::Requested,
            output_secret: None,
            evidence: None,
            failure: None,
            purge_audit_intent_digest: None,
        };
        claim.validate()?;
        Ok(claim)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != RESOURCE_CLAIM_SCHEMA_VERSION {
            return Err(ResourceClaimError::UnsupportedSchemaVersion {
                actual: self.schema_version.clone(),
            });
        }
        if self.generation == 0 {
            return Err(ResourceClaimError::InvalidGeneration);
        }
        validate_identity(&self.identity)?;
        validate_identifier("providerId", &self.provider_id)?;
        let expected = resource_claim_digest(&self.identity)?;
        if expected != self.claim_digest {
            return Err(ResourceClaimError::ClaimDigestMismatch {
                expected,
                actual: self.claim_digest.clone(),
            });
        }
        if self.identity.resource_type != RESOURCE_TYPE_POSTGRESQL_DATABASE {
            return Err(ResourceClaimError::UnsupportedResourceType(
                self.identity.resource_type.clone(),
            ));
        }
        if let Some(output) = &self.output_secret {
            validate_secret_reference(&output.reference)?;
            validate_digest("outputSecret.contentDigest", &output.content_digest)?;
            if output.mode != OUTPUT_SECRET_MODE {
                return Err(ResourceClaimError::InvalidSecretEvidence(format!(
                    "mode must be {:04o}",
                    OUTPUT_SECRET_MODE
                )));
            }
            if output.generation != self.generation {
                return Err(ResourceClaimError::InvalidSecretEvidence(
                    "output generation does not match claim generation".to_string(),
                ));
            }
        }
        if let Some(evidence) = &self.evidence {
            validate_evidence_identity(self, evidence)?;
        }
        if let Some(digest) = &self.purge_audit_intent_digest {
            validate_digest("purgeAuditIntentDigest", digest)?;
        }
        match self.status {
            ResourceClaimStatusV1::Ready if self.output_secret.is_none() => {
                return Err(ResourceClaimError::InvalidSecretEvidence(
                    "READY claim requires an output secret reference".to_string(),
                ));
            }
            ResourceClaimStatusV1::Retained | ResourceClaimStatusV1::Deleted
                if self.output_secret.is_some() =>
            {
                return Err(ResourceClaimError::InvalidSecretEvidence(
                    "RETAINED/DELETED claim cannot retain an output DSN binding".to_string(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    pub fn postgres_names(&self) -> Result<PostgreSqlNamesV1> {
        self.validate()?;
        postgres_names(&self.claim_digest)
    }

    pub fn output_secret_reference(&self) -> String {
        format!(
            "agent-secret://resource-outputs/{}/g{}/dsn",
            digest_hex(&self.claim_digest),
            self.generation
        )
    }

    pub fn credential_secret_key(&self) -> String {
        format!(
            "resource-credential:{}/g{}",
            digest_hex(&self.claim_digest),
            self.generation
        )
    }

    pub fn purge_confirmation(&self) -> String {
        format!(
            "PURGE {} {} GENERATION {}",
            self.identity.claim_id, self.claim_digest, self.generation
        )
    }
}

pub fn resource_claim_digest(identity: &ResourceClaimIdentityV1) -> Result<String> {
    validate_identity(identity)?;
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"ojos.dev/resource-claim/identity/v1");
    for value in [
        identity.claim_id.as_bytes(),
        identity.owner_instance_id.as_bytes(),
        identity.service_id.as_bytes(),
        identity.resource_name.as_bytes(),
        identity.resource_type.as_bytes(),
    ] {
        hash_field(&mut hasher, value);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostgreSqlNamesV1 {
    pub database_name: String,
    pub role_name: String,
}

pub fn postgres_names(claim_digest: &str) -> Result<PostgreSqlNamesV1> {
    validate_digest("claimDigest", claim_digest)?;
    let suffix = &digest_hex(claim_digest)[..52];
    let names = PostgreSqlNamesV1 {
        database_name: format!("ojosdb_{suffix}"),
        role_name: format!("ojosrole_{suffix}"),
    };
    debug_assert!(names.database_name.len() <= 63);
    debug_assert!(names.role_name.len() <= 63);
    Ok(names)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostgreSqlProviderDescriptorV1 {
    pub provider_id: String,
    pub host: String,
    pub port: u16,
    pub tls_mode: PostgreSqlTlsModeV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PostgreSqlTlsModeV1 {
    Require,
    VerifyCa,
    VerifyFull,
}

impl PostgreSqlProviderDescriptorV1 {
    pub fn validate(&self) -> Result<()> {
        validate_identifier("provider.providerId", &self.provider_id)?;
        if self.port == 0 {
            return Err(ResourceClaimError::InvalidProvider(
                "port must be greater than zero".to_string(),
            ));
        }
        if !valid_host(&self.host) {
            return Err(ResourceClaimError::InvalidProvider(
                "host must be a hostname or IP address without credentials or a URL".to_string(),
            ));
        }
        Ok(())
    }

    fn sslmode(&self) -> &'static str {
        match self.tls_mode {
            PostgreSqlTlsModeV1::Require => "require",
            PostgreSqlTlsModeV1::VerifyCa => "verify-ca",
            PostgreSqlTlsModeV1::VerifyFull => "verify-full",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResourceClaimActionKindV1 {
    Ensure,
    Inspect,
    Release,
    Purge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "action",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum ResourceClaimActionV1 {
    Ensure,
    Inspect,
    Release,
    Purge { authorization: PurgeAuthorizationV1 },
}

impl ResourceClaimActionV1 {
    pub fn kind(&self) -> ResourceClaimActionKindV1 {
        match self {
            Self::Ensure => ResourceClaimActionKindV1::Ensure,
            Self::Inspect => ResourceClaimActionKindV1::Inspect,
            Self::Release => ResourceClaimActionKindV1::Release,
            Self::Purge { .. } => ResourceClaimActionKindV1::Purge,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PurgeAuthorizationV1 {
    pub confirmation: String,
    pub audit_intent: PurgeAuditIntentV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PurgeAuditIntentV1 {
    pub intent_id: String,
    pub actor_id: String,
    pub reason: String,
    pub claim_digest: String,
    pub generation: u64,
}

impl PurgeAuditIntentV1 {
    pub fn digest(&self) -> Result<String> {
        self.validate_shape()?;
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, b"ojos.dev/resource-purge-audit-intent/v1");
        for value in [
            self.intent_id.as_bytes(),
            self.actor_id.as_bytes(),
            self.reason.as_bytes(),
            self.claim_digest.as_bytes(),
            self.generation.to_string().as_bytes(),
        ] {
            hash_field(&mut hasher, value);
        }
        Ok(format!("sha256:{:x}", hasher.finalize()))
    }

    fn validate_for(&self, claim: &ResourceClaimV1) -> Result<()> {
        self.validate_shape()?;
        if self.claim_digest != claim.claim_digest || self.generation != claim.generation {
            return Err(ResourceClaimError::InvalidPurgeAuditIntent(
                "claim digest and generation must match the purge target".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        validate_identifier("auditIntent.intentId", &self.intent_id)?;
        validate_actor_id(&self.actor_id)?;
        validate_digest("auditIntent.claimDigest", &self.claim_digest)?;
        if self.generation == 0 {
            return Err(ResourceClaimError::InvalidPurgeAuditIntent(
                "generation must be greater than zero".to_string(),
            ));
        }
        let reason = self.reason.trim();
        if reason.len() < 8 || reason.len() > 512 || reason != self.reason {
            return Err(ResourceClaimError::InvalidPurgeAuditIntent(
                "reason must contain 8..512 characters without surrounding whitespace".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostgreSqlCommandV1 {
    pub action: ResourceClaimActionKindV1,
    pub claim_digest: String,
    pub generation: u64,
    pub provider_id: String,
    pub database_name: String,
    pub role_name: String,
    pub idempotency_key: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_audit_intent_digest: Option<String>,
    pub steps: Vec<PostgreSqlPlanStepV1>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PostgreSqlPlanStepV1 {
    InspectDatabase,
    InspectRole,
    InspectOwnership,
    EnsureRoleLoginWithCredential,
    EnsureDatabaseOwnedByRole,
    EnsureDatabasePrivileges,
    DisableRoleLogin,
    RevokeDatabaseConnections,
    TerminateDatabaseSessions,
    DropDatabase,
    DropRole,
}

impl PostgreSqlCommandV1 {
    pub fn validate(&self) -> Result<()> {
        validate_digest("command.claimDigest", &self.claim_digest)?;
        validate_identifier("command.providerId", &self.provider_id)?;
        validate_postgres_name("command.databaseName", &self.database_name)?;
        validate_postgres_name("command.roleName", &self.role_name)?;
        if self.generation == 0 {
            return Err(ResourceClaimError::InvalidGeneration);
        }
        let expected_key = idempotency_key(&self.claim_digest, self.generation, self.action);
        if self.idempotency_key != expected_key {
            return Err(ResourceClaimError::Provider(
                "command idempotency key does not match claim and generation".to_string(),
            ));
        }
        validate_digest("command.requestDigest", &self.request_digest)?;
        if let Some(digest) = &self.credential_digest {
            validate_digest("command.credentialDigest", digest)?;
        }
        if let Some(digest) = &self.purge_audit_intent_digest {
            validate_digest("command.purgeAuditIntentDigest", digest)?;
        }
        let expected = command_request_digest(self)?;
        if expected != self.request_digest {
            return Err(ResourceClaimError::Provider(
                "command request digest mismatch".to_string(),
            ));
        }
        Ok(())
    }
}

pub fn plan_postgresql_command(
    claim: &ResourceClaimV1,
    action: ResourceClaimActionKindV1,
    credential_digest: Option<String>,
    purge_audit_intent_digest: Option<String>,
) -> Result<PostgreSqlCommandV1> {
    claim.validate()?;
    let names = claim.postgres_names()?;
    let steps = match action {
        ResourceClaimActionKindV1::Ensure => {
            if credential_digest.is_none() || purge_audit_intent_digest.is_some() {
                return Err(ResourceClaimError::Provider(
                    "ENSURE requires credential digest and forbids purge audit intent".to_string(),
                ));
            }
            vec![
                PostgreSqlPlanStepV1::InspectDatabase,
                PostgreSqlPlanStepV1::InspectRole,
                PostgreSqlPlanStepV1::EnsureRoleLoginWithCredential,
                PostgreSqlPlanStepV1::EnsureDatabaseOwnedByRole,
                PostgreSqlPlanStepV1::EnsureDatabasePrivileges,
                PostgreSqlPlanStepV1::InspectOwnership,
            ]
        }
        ResourceClaimActionKindV1::Inspect => vec![
            PostgreSqlPlanStepV1::InspectDatabase,
            PostgreSqlPlanStepV1::InspectRole,
            PostgreSqlPlanStepV1::InspectOwnership,
        ],
        ResourceClaimActionKindV1::Release => {
            if credential_digest.is_some() || purge_audit_intent_digest.is_some() {
                return Err(ResourceClaimError::Provider(
                    "RELEASE cannot carry credential or purge material".to_string(),
                ));
            }
            vec![
                PostgreSqlPlanStepV1::InspectDatabase,
                PostgreSqlPlanStepV1::InspectRole,
                PostgreSqlPlanStepV1::DisableRoleLogin,
                PostgreSqlPlanStepV1::InspectOwnership,
            ]
        }
        ResourceClaimActionKindV1::Purge => {
            if credential_digest.is_some() || purge_audit_intent_digest.is_none() {
                return Err(ResourceClaimError::Provider(
                    "PURGE requires audit intent digest and forbids credential material"
                        .to_string(),
                ));
            }
            vec![
                PostgreSqlPlanStepV1::RevokeDatabaseConnections,
                PostgreSqlPlanStepV1::TerminateDatabaseSessions,
                PostgreSqlPlanStepV1::DropDatabase,
                PostgreSqlPlanStepV1::DropRole,
                PostgreSqlPlanStepV1::InspectDatabase,
                PostgreSqlPlanStepV1::InspectRole,
            ]
        }
    };
    let mut command = PostgreSqlCommandV1 {
        action,
        claim_digest: claim.claim_digest.clone(),
        generation: claim.generation,
        provider_id: claim.provider_id.clone(),
        database_name: names.database_name,
        role_name: names.role_name,
        idempotency_key: idempotency_key(&claim.claim_digest, claim.generation, action),
        request_digest: String::new(),
        credential_digest,
        purge_audit_intent_digest,
        steps,
    };
    command.request_digest = command_request_digest(&command)?;
    command.validate()?;
    Ok(command)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PostgreSqlEvidenceV1 {
    pub claim_digest: String,
    pub generation: u64,
    pub provider_id: String,
    pub database_name: String,
    pub role_name: String,
    pub database_exists: bool,
    pub role_exists: bool,
    pub owner_matches: bool,
    pub role_can_login: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_audit_intent_digest: Option<String>,
    pub evidence_digest: String,
}

impl PostgreSqlEvidenceV1 {
    pub fn seal(mut self) -> Result<Self> {
        self.evidence_digest = evidence_digest(&self)?;
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<()> {
        validate_digest("evidence.claimDigest", &self.claim_digest)?;
        validate_identifier("evidence.providerId", &self.provider_id)?;
        validate_postgres_name("evidence.databaseName", &self.database_name)?;
        validate_postgres_name("evidence.roleName", &self.role_name)?;
        if self.generation == 0 {
            return Err(ResourceClaimError::InvalidGeneration);
        }
        if let Some(digest) = &self.credential_digest {
            validate_digest("evidence.credentialDigest", digest)?;
        }
        if let Some(digest) = &self.purge_audit_intent_digest {
            validate_digest("evidence.purgeAuditIntentDigest", digest)?;
        }
        validate_digest("evidence.evidenceDigest", &self.evidence_digest)?;
        let expected = evidence_digest(self)?;
        if expected != self.evidence_digest {
            return Err(ResourceClaimError::InvalidEvidence(
                "evidence digest mismatch".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "outcome",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum PostgreSqlExecutionOutcomeV1 {
    Applied { evidence: PostgreSqlEvidenceV1 },
    Replayed { evidence: PostgreSqlEvidenceV1 },
    InProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureKindV1 {
    Retryable,
    Rejected,
    FactUnknown,
    IdempotencyConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExecutionErrorV1 {
    pub kind: ProviderFailureKindV1,
    pub code: String,
}

impl fmt::Display for ProviderExecutionErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.code, self.kind)
    }
}

pub trait PostgreSqlCommandExecutor: Send + Sync {
    /// Execute atomically with respect to `command.idempotency_key`.
    ///
    /// Same key + same request digest must replay the exact evidence. Same key +
    /// different digest must return `IdempotencyConflict`. The optional secret is
    /// required only for `EnsureRoleLoginWithCredential`; implementations obtain
    /// administrator credentials from node-local configuration, not this request.
    fn execute(
        &self,
        command: &PostgreSqlCommandV1,
        credential: Option<&SecretMaterial>,
    ) -> std::result::Result<PostgreSqlExecutionOutcomeV1, ProviderExecutionErrorV1>;
}

pub trait CryptographicPasswordGenerator: Send + Sync {
    /// Fill every byte from an operating-system cryptographic random source.
    fn fill_random(&self, destination: &mut [u8]) -> std::result::Result<(), String>;
}

pub struct SecretMaterial {
    bytes: Vec<u8>,
}

impl SecretMaterial {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() {
            return Err(ResourceClaimError::SecretStore(
                "secret material cannot be empty".to_string(),
            ));
        }
        Ok(Self { bytes })
    }

    /// This access is intentionally explicit and should remain inside an Agent
    /// executor or secret sink implementation.
    pub fn expose_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn digest(&self) -> String {
        format!("sha256:{:x}", Sha256::digest(&self.bytes))
    }

    fn random_password<G: CryptographicPasswordGenerator>(generator: &G) -> Result<Self> {
        let mut random = [0_u8; GENERATED_PASSWORD_BYTES];
        generator
            .fill_random(&mut random)
            .map_err(ResourceClaimError::PasswordGeneration)?;
        let mut encoded = Vec::with_capacity(GENERATED_PASSWORD_BYTES * 2);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in random {
            encoded.push(HEX[(byte >> 4) as usize]);
            encoded.push(HEX[(byte & 0x0f) as usize]);
        }
        random.fill(0);
        Self::new(encoded)
    }
}

impl Clone for SecretMaterial {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
        }
    }
}

impl fmt::Debug for SecretMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretMaterial")
            .field("bytes", &"[REDACTED]")
            .field("digest", &self.digest())
            .finish()
    }
}

impl Drop for SecretMaterial {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretWriteEvidenceV1 {
    pub reference: String,
    pub content_digest: String,
    pub mode: u32,
}

pub trait ResourceSecretStore: Send + Sync {
    /// Atomically create or return the existing node-local credential for `key`.
    /// Existing material wins during concurrent calls.
    fn get_or_create_0600(
        &self,
        key: &str,
        candidate: SecretMaterial,
    ) -> std::result::Result<SecretMaterial, String>;

    fn read_0600(&self, key: &str) -> std::result::Result<Option<SecretMaterial>, String>;

    /// Atomically create the immutable output secret, or verify an exact replay,
    /// and return evidence of mode and digest. Different existing bytes fail.
    fn write_output_0600(
        &self,
        reference: &str,
        material: SecretMaterial,
    ) -> std::result::Result<SecretWriteEvidenceV1, String>;

    fn remove_output(&self, reference: &str) -> std::result::Result<(), String>;

    fn remove_internal(&self, key: &str) -> std::result::Result<(), String>;
}

/// Durable Node-local secret sink.  References are opaque identifiers; their
/// path is derived below the configured root and never accepted from a Job.
/// Internal credential creation is immutable and serialized by a private lock
/// file so concurrent/restarted Ensures recover the same password. Output
/// documents are also immutable: the same bytes replay, different bytes fail.
#[derive(Debug, Clone)]
pub struct FileResourceSecretStore {
    internal_root: PathBuf,
    output_root: PathBuf,
    workload_file_ownership: WorkloadFileOwnership,
}

impl FileResourceSecretStore {
    pub fn new(root: impl Into<PathBuf>) -> std::result::Result<Self, String> {
        Self::new_with_ownership(root, WorkloadFileOwnership::current_process())
    }

    pub fn new_with_ownership(
        root: impl Into<PathBuf>,
        workload_file_ownership: WorkloadFileOwnership,
    ) -> std::result::Result<Self, String> {
        let root = root.into();
        Self::new_isolated_with_ownership(
            root.join("internal"),
            root.join("outputs"),
            workload_file_ownership,
        )
    }

    /// Create a store whose generated provider credential and workload-visible
    /// output are held below disjoint roots.  Only `output_root` may be exposed
    /// read-only to a Docker daemon namespace.
    pub fn new_isolated_with_ownership(
        internal_root: impl Into<PathBuf>,
        output_root: impl Into<PathBuf>,
        workload_file_ownership: WorkloadFileOwnership,
    ) -> std::result::Result<Self, String> {
        validate_workload_file_ownership(workload_file_ownership)?;
        let internal_root = internal_root.into();
        let output_root = output_root.into();
        validate_isolated_secret_roots(&internal_root, &output_root)?;
        let store = Self {
            internal_root,
            output_root,
            workload_file_ownership,
        };
        create_private_directory(&store.internal_root, workload_file_ownership)?;
        create_private_directory(&store.output_root, workload_file_ownership)?;
        validate_isolated_secret_roots(&store.internal_root, &store.output_root)?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.internal_root
    }

    pub fn output_root(&self) -> &Path {
        &self.output_root
    }

    /// Resolve an output reference for mounting or environment indirection.
    /// The returned path is Node-local and must never be serialized back to the
    /// control-plane result.
    pub fn output_path(&self, reference: &str) -> std::result::Result<PathBuf, String> {
        let component = output_reference_component(reference)?;
        Ok(self.output_root.join(component))
    }

    pub fn read_output(
        &self,
        reference: &str,
    ) -> std::result::Result<Option<SecretMaterial>, String> {
        read_private_optional(&self.output_path(reference)?, self.workload_file_ownership)?
            .map(SecretMaterial::new)
            .transpose()
            .map_err(|error| error.to_string())
    }

    fn internal_path(&self, key: &str) -> std::result::Result<PathBuf, String> {
        validate_secret_key(key)?;
        Ok(self.internal_root.join(secret_component(key)))
    }

    fn with_lock<T>(
        &self,
        lock_key: &str,
        action: impl FnOnce() -> std::result::Result<T, String>,
    ) -> std::result::Result<T, String> {
        let lock_path = self
            .internal_root
            .join(format!("{}.lock", secret_component(lock_key)));
        let lock = open_private_lock(&lock_path, self.workload_file_ownership)?;
        lock.lock_exclusive()
            .map_err(|error| format!("lock private secret state: {error}"))?;
        let result = action();
        let unlock =
            FileExt::unlock(&lock).map_err(|error| format!("unlock private secret state: {error}"));
        match (result, unlock) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }
}

impl ResourceSecretStore for FileResourceSecretStore {
    fn get_or_create_0600(
        &self,
        key: &str,
        candidate: SecretMaterial,
    ) -> std::result::Result<SecretMaterial, String> {
        let path = self.internal_path(key)?;
        self.with_lock(&format!("credential:{key}"), || {
            if let Some(existing) = read_private_optional(&path, self.workload_file_ownership)? {
                return SecretMaterial::new(existing).map_err(|error| error.to_string());
            }
            write_private_new(
                &path,
                candidate.expose_bytes(),
                self.workload_file_ownership,
            )?;
            SecretMaterial::new(candidate.expose_bytes().to_vec())
                .map_err(|error| error.to_string())
        })
    }

    fn read_0600(&self, key: &str) -> std::result::Result<Option<SecretMaterial>, String> {
        let path = self.internal_path(key)?;
        read_private_optional(&path, self.workload_file_ownership)?
            .map(SecretMaterial::new)
            .transpose()
            .map_err(|error| error.to_string())
    }

    fn write_output_0600(
        &self,
        reference: &str,
        material: SecretMaterial,
    ) -> std::result::Result<SecretWriteEvidenceV1, String> {
        let path = self.output_path(reference)?;
        self.with_lock(&format!("output:{reference}"), || {
            if let Some(existing) = read_private_optional(&path, self.workload_file_ownership)? {
                if existing != material.expose_bytes() {
                    return Err(
                        "immutable resource output already exists with different bytes".to_string(),
                    );
                }
            } else {
                write_private_new(&path, material.expose_bytes(), self.workload_file_ownership)?;
            }
            Ok(SecretWriteEvidenceV1 {
                reference: reference.to_string(),
                content_digest: material.digest(),
                mode: OUTPUT_SECRET_MODE,
            })
        })
    }

    fn remove_output(&self, reference: &str) -> std::result::Result<(), String> {
        let path = self.output_path(reference)?;
        remove_private_file(&path)
    }

    fn remove_internal(&self, key: &str) -> std::result::Result<(), String> {
        let path = self.internal_path(key)?;
        remove_private_file(&path)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OsCryptographicPasswordGenerator;

impl CryptographicPasswordGenerator for OsCryptographicPasswordGenerator {
    fn fill_random(&self, destination: &mut [u8]) -> std::result::Result<(), String> {
        getrandom::fill(destination).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum PostgreSqlTlsTrustV1 {
    Platform,
    CaCertificate(PathBuf),
}

/// Agent-local administrator connection configuration.  The URL is never
/// copied into a claim, Job result, evidence, or error string.
#[derive(Clone)]
pub struct PostgreSqlAdminConfigV1 {
    pub provider: PostgreSqlProviderDescriptorV1,
    pub admin_url: SecretMaterial,
    pub tls_trust: PostgreSqlTlsTrustV1,
    pub state_database: PathBuf,
}

impl fmt::Debug for PostgreSqlAdminConfigV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgreSqlAdminConfigV1")
            .field("provider", &self.provider)
            .field("admin_url", &"[REDACTED]")
            .field("tls_trust", &self.tls_trust)
            .field("state_database", &self.state_database)
            .finish()
    }
}

/// Real PostgreSQL executor. Each replay is protected by an Agent-local
/// idempotency receipt, while PostgreSQL catalog inspection makes a lost
/// response safe even when the receipt was not committed after the server
/// mutation.
pub struct LivePostgreSqlExecutor {
    config: PostgreSqlAdminConfigV1,
    receipts: Mutex<Connection>,
}

pub trait ResourceClaimPipelineExecutor: Send + Sync {
    fn ensure(
        &self,
        step: &orchestrator_runtime::ResourceClaimStepV1,
    ) -> std::result::Result<ResourceClaimV1, ResourceClaimError>;

    fn release_deployment(
        &self,
        deployment_id: &str,
    ) -> std::result::Result<Vec<ResourceClaimReleaseResultV1>, ResourceClaimError>;

    /// Validate that a replacement reuses exactly the old deployment's stable
    /// claims. This is read-only so failed replacement attempts leave the old
    /// binding authoritative.
    fn reuse_for_replacement(
        &self,
        old_deployment_id: &str,
        steps: &[orchestrator_runtime::ResourceClaimStepV1],
    ) -> std::result::Result<Vec<ResourceClaimV1>, ResourceClaimError>;

    /// Add the healthy replacement binding. The old binding remains until the
    /// old container is actually removed (or its explicit Uninstall runs).
    fn bind_replacement(
        &self,
        old_deployment_id: &str,
        new_deployment_id: &str,
        claim_ids: &[String],
    ) -> std::result::Result<(), ResourceClaimError>;

    /// Execute an explicit destructive action against one Agent-local durable
    /// claim. Implementations must reject every live deployment binding before
    /// contacting the provider.
    fn purge(
        &self,
        payload: &orchestrator_runtime::ResourcePurgePayloadV1,
    ) -> std::result::Result<ResourceClaimV1, ResourceClaimError> {
        let _ = payload;
        Err(ResourceClaimError::Provider(
            "resource purge is not configured on this Agent".to_string(),
        ))
    }

    fn output_path(&self, reference: &str) -> std::result::Result<PathBuf, ResourceClaimError>;
}

/// Async boundary for the Agent's synchronous ResourceClaim backend.
///
/// The concrete PostgreSQL client owns a synchronous Tokio runtime internally,
/// and the claim manager also performs blocking SQLite and filesystem I/O. All
/// calls therefore have to leave the Agent worker runtime before entering the
/// backend. Keeping that rule in one handle prevents an individual job path
/// from accidentally calling the synchronous provider on an async worker.
#[derive(Clone)]
pub(crate) struct ResourceClaimPipelineHandle {
    backend: Arc<dyn ResourceClaimPipelineExecutor>,
}

impl ResourceClaimPipelineHandle {
    pub(crate) fn new(backend: Arc<dyn ResourceClaimPipelineExecutor>) -> Self {
        Self { backend }
    }

    async fn execute<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn ResourceClaimPipelineExecutor>) -> Result<T> + Send + 'static,
    {
        let backend = Arc::clone(&self.backend);
        tokio::task::spawn_blocking(move || operation(backend))
            .await
            .map_err(|_| ResourceClaimError::ExecutionOutcomeUnknown)?
    }

    pub(crate) async fn ensure(
        &self,
        step: &orchestrator_runtime::ResourceClaimStepV1,
    ) -> Result<ResourceClaimV1> {
        let step = step.clone();
        self.execute(move |backend| backend.ensure(&step)).await
    }

    pub(crate) async fn release_deployment(
        &self,
        deployment_id: &str,
    ) -> Result<Vec<ResourceClaimReleaseResultV1>> {
        let deployment_id = deployment_id.to_string();
        self.execute(move |backend| backend.release_deployment(&deployment_id))
            .await
    }

    pub(crate) async fn reuse_for_replacement(
        &self,
        old_deployment_id: &str,
        steps: &[orchestrator_runtime::ResourceClaimStepV1],
    ) -> Result<Vec<ResourceClaimV1>> {
        let old_deployment_id = old_deployment_id.to_string();
        let steps = steps.to_vec();
        self.execute(move |backend| backend.reuse_for_replacement(&old_deployment_id, &steps))
            .await
    }

    pub(crate) async fn bind_replacement(
        &self,
        old_deployment_id: &str,
        new_deployment_id: &str,
        claim_ids: &[String],
    ) -> Result<()> {
        let old_deployment_id = old_deployment_id.to_string();
        let new_deployment_id = new_deployment_id.to_string();
        let claim_ids = claim_ids.to_vec();
        self.execute(move |backend| {
            backend.bind_replacement(&old_deployment_id, &new_deployment_id, &claim_ids)
        })
        .await
    }

    pub(crate) async fn purge(
        &self,
        payload: &orchestrator_runtime::ResourcePurgePayloadV1,
    ) -> Result<ResourceClaimV1> {
        let payload = payload.clone();
        self.execute(move |backend| backend.purge(&payload)).await
    }

    pub(crate) async fn output_path(&self, reference: &str) -> Result<PathBuf> {
        let reference = reference.to_string();
        self.execute(move |backend| backend.output_path(&reference))
            .await
    }
}

/// Durable pipeline bridge for the pure claim state machine.  Claim records
/// live only in the Agent-local SQLite database and contain no credentials.
pub struct LocalResourceClaimManager<E = LivePostgreSqlExecutor> {
    provider: PostgreSqlProviderDescriptorV1,
    executor: E,
    secrets: FileResourceSecretStore,
    state: Mutex<Connection>,
    random: OsCryptographicPasswordGenerator,
}

#[derive(Debug, Clone)]
pub struct ResourceClaimReleaseResultV1 {
    pub claim: ResourceClaimV1,
    /// True when this uninstall removed the final runtime binding and the
    /// provider was transitioned to RETAINED. False means another deployment
    /// still consumes the same READY claim.
    pub provider_released: bool,
}

impl<E> LocalResourceClaimManager<E>
where
    E: PostgreSqlCommandExecutor,
{
    pub fn new(
        provider: PostgreSqlProviderDescriptorV1,
        executor: E,
        secrets: FileResourceSecretStore,
        state_database: impl AsRef<Path>,
    ) -> std::result::Result<Self, ResourceClaimError> {
        if let Some(parent) = state_database
            .as_ref()
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        }
        let state = Connection::open(state_database)
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        state
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        state.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;\
             CREATE TABLE IF NOT EXISTS resource_claim_state (\
               claim_id TEXT PRIMARY KEY, owner_instance_id TEXT NOT NULL, service_id TEXT NOT NULL, resource_name TEXT NOT NULL, claim_json TEXT NOT NULL, updated_at_ms INTEGER NOT NULL,\
               UNIQUE(owner_instance_id, service_id, resource_name)\
             );\
             CREATE TABLE IF NOT EXISTS resource_claim_bindings (\
               claim_id TEXT NOT NULL, deployment_id TEXT NOT NULL, created_at_ms INTEGER NOT NULL,\
               PRIMARY KEY(claim_id, deployment_id),\
               FOREIGN KEY(claim_id) REFERENCES resource_claim_state(claim_id) ON DELETE RESTRICT\
             );\
             CREATE INDEX IF NOT EXISTS idx_resource_claim_binding_deployment ON resource_claim_bindings(deployment_id, claim_id);",
        ).map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        Ok(Self {
            provider,
            executor,
            secrets,
            state: Mutex::new(state),
            random: OsCryptographicPasswordGenerator,
        })
    }

    fn load(&self, claim_id: &str) -> Result<Option<ResourceClaimV1>> {
        let state = self
            .state
            .lock()
            .map_err(|_| ResourceClaimError::Provider("claim state lock poisoned".to_string()))?;
        let json = state
            .query_row(
                "SELECT claim_json FROM resource_claim_state WHERE claim_id=?1",
                params![claim_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        json.map(|json| {
            serde_json::from_str(&json)
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))
        })
        .transpose()
    }

    fn save(&self, claim: &ResourceClaimV1) -> Result<()> {
        let json = serde_json::to_string(claim)
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        let state = self
            .state
            .lock()
            .map_err(|_| ResourceClaimError::Provider("claim state lock poisoned".to_string()))?;
        state.execute(
            "INSERT INTO resource_claim_state (claim_id,owner_instance_id,service_id,resource_name,claim_json,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6)\
             ON CONFLICT(claim_id) DO UPDATE SET owner_instance_id=excluded.owner_instance_id, service_id=excluded.service_id, resource_name=excluded.resource_name, claim_json=excluded.claim_json, updated_at_ms=excluded.updated_at_ms",
            params![claim.identity.claim_id, claim.identity.owner_instance_id, claim.identity.service_id, claim.identity.resource_name, json, crate::now_ms()],
        ).map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        Ok(())
    }

    fn bind(&self, claim_id: &str, deployment_id: &str) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| ResourceClaimError::Provider("claim state lock poisoned".to_string()))?;
        state
            .execute(
                "INSERT OR IGNORE INTO resource_claim_bindings (claim_id,deployment_id,created_at_ms) VALUES (?1,?2,?3)",
                params![claim_id, deployment_id, crate::now_ms()],
            )
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        Ok(())
    }

    fn claimed(&self, step: &orchestrator_runtime::ResourceClaimStepV1) -> Result<ResourceClaimV1> {
        step.validate()
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        if let Some(existing) = self.load(&step.claim_id)? {
            if existing.identity.owner_instance_id != step.owner_instance_id
                || existing.identity.service_id != step.service_id
                || existing.identity.resource_name != step.resource_name
                || existing.generation != step.generation
                || existing.provider_id != step.provider_id
            {
                return Err(ResourceClaimError::Provider(
                    "resource claim identity/generation conflicts with durable Agent state"
                        .to_string(),
                ));
            }
            return Ok(existing);
        }
        let conflicting_claim = {
            let state = self.state.lock().map_err(|_| {
                ResourceClaimError::Provider("claim state lock poisoned".to_string())
            })?;
            state
                .query_row(
                    "SELECT claim_id FROM resource_claim_state WHERE owner_instance_id=?1 AND service_id=?2 AND resource_name=?3",
                    params![step.owner_instance_id, step.service_id, step.resource_name],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
        };
        if let Some(conflicting_claim) = conflicting_claim {
            return Err(ResourceClaimError::Provider(format!(
                "stable resource owner already maps to claim {conflicting_claim}; refusing a second claim/database"
            )));
        }
        ResourceClaimV1::requested(
            ResourceClaimIdentityV1 {
                claim_id: step.claim_id.clone(),
                owner_instance_id: step.owner_instance_id.clone(),
                service_id: step.service_id.clone(),
                resource_name: step.resource_name.clone(),
                resource_type: RESOURCE_TYPE_POSTGRESQL_DATABASE.to_string(),
            },
            step.generation,
            step.provider_id.clone(),
        )
    }
}

impl<E> ResourceClaimPipelineExecutor for LocalResourceClaimManager<E>
where
    E: PostgreSqlCommandExecutor,
{
    fn ensure(&self, step: &orchestrator_runtime::ResourceClaimStepV1) -> Result<ResourceClaimV1> {
        let current = self.claimed(step)?;
        let next = if current.status == ResourceClaimStatusV1::Ready {
            // A durable READY claim already has immutable provider evidence and
            // an immutable output. Rebinding a release must not rotate its
            // credential or mutate PostgreSQL again.
            current
        } else if current.status == ResourceClaimStatusV1::NeedsAttention {
            execute_resource_claim(
                current,
                ResourceClaimActionV1::Inspect,
                &self.provider,
                &self.executor,
                &self.secrets,
                &self.random,
            )?
        } else {
            execute_resource_claim(
                current,
                ResourceClaimActionV1::Ensure,
                &self.provider,
                &self.executor,
                &self.secrets,
                &self.random,
            )?
        };
        self.save(&next)?;
        if next.status == ResourceClaimStatusV1::Ready {
            self.bind(&next.identity.claim_id, &step.deployment_id)?;
        }
        Ok(next)
    }

    fn release_deployment(&self, deployment_id: &str) -> Result<Vec<ResourceClaimReleaseResultV1>> {
        let claims = {
            let state = self.state.lock().map_err(|_| {
                ResourceClaimError::Provider("claim state lock poisoned".to_string())
            })?;
            let mut statement = state.prepare(
                "SELECT state.claim_json FROM resource_claim_state state JOIN resource_claim_bindings binding ON binding.claim_id=state.claim_id WHERE binding.deployment_id=?1 ORDER BY state.claim_id",
            ).map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
            statement
                .query_map(params![deployment_id], |row| row.get::<_, String>(0))
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
        };
        let mut released = Vec::with_capacity(claims.len());
        for json in claims {
            let claim: ResourceClaimV1 = serde_json::from_str(&json)
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
            let other_bindings = {
                let state = self.state.lock().map_err(|_| {
                    ResourceClaimError::Provider("claim state lock poisoned".to_string())
                })?;
                state
                    .query_row(
                        "SELECT COUNT(*) FROM resource_claim_bindings WHERE claim_id=?1 AND deployment_id<>?2",
                        params![claim.identity.claim_id, deployment_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
            };
            let claim = if other_bindings == 0
                && matches!(
                    claim.status,
                    ResourceClaimStatusV1::Ready | ResourceClaimStatusV1::Releasing
                ) {
                execute_resource_claim(
                    claim,
                    ResourceClaimActionV1::Release,
                    &self.provider,
                    &self.executor,
                    &self.secrets,
                    &self.random,
                )?
            } else {
                claim
            };
            self.save(&claim)?;
            let state = self.state.lock().map_err(|_| {
                ResourceClaimError::Provider("claim state lock poisoned".to_string())
            })?;
            state
                .execute(
                    "DELETE FROM resource_claim_bindings WHERE claim_id=?1 AND deployment_id=?2",
                    params![claim.identity.claim_id, deployment_id],
                )
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
            released.push(ResourceClaimReleaseResultV1 {
                claim,
                provider_released: other_bindings == 0,
            });
        }
        Ok(released)
    }

    fn reuse_for_replacement(
        &self,
        old_deployment_id: &str,
        steps: &[orchestrator_runtime::ResourceClaimStepV1],
    ) -> Result<Vec<ResourceClaimV1>> {
        let old_claim_ids = {
            let state = self.state.lock().map_err(|_| {
                ResourceClaimError::Provider("claim state lock poisoned".to_string())
            })?;
            let mut statement = state
                .prepare("SELECT claim_id FROM resource_claim_bindings WHERE deployment_id=?1 ORDER BY claim_id")
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
            statement
                .query_map(params![old_deployment_id], |row| row.get::<_, String>(0))
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
        };
        let mut desired_claim_ids = steps
            .iter()
            .map(|step| step.claim_id.clone())
            .collect::<Vec<_>>();
        desired_claim_ids.sort();
        if old_claim_ids != desired_claim_ids {
            return Err(ResourceClaimError::Provider(format!(
                "replacement ResourceClaim set differs from old deployment {old_deployment_id}; refusing implicit resource creation/removal"
            )));
        }
        let mut claims = Vec::with_capacity(steps.len());
        for step in steps {
            step.validate()
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
            let claim = self.load(&step.claim_id)?.ok_or_else(|| {
                ResourceClaimError::Provider(format!(
                    "replacement claim {} is not durable on this Agent",
                    step.claim_id
                ))
            })?;
            if claim.identity.owner_instance_id != step.owner_instance_id
                || claim.identity.service_id != step.service_id
                || claim.identity.resource_name != step.resource_name
                || claim.identity.resource_type != step.resource_type
                || claim.provider_id != step.provider_id
                || claim.generation != step.generation
                || claim.status != ResourceClaimStatusV1::Ready
            {
                return Err(ResourceClaimError::Provider(format!(
                    "replacement claim {} changed stable identity/provider/generation or is not READY",
                    step.claim_id
                )));
            }
            claims.push(claim);
        }
        Ok(claims)
    }

    fn bind_replacement(
        &self,
        old_deployment_id: &str,
        new_deployment_id: &str,
        claim_ids: &[String],
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| ResourceClaimError::Provider("claim state lock poisoned".to_string()))?;
        let transaction = state
            .unchecked_transaction()
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        for claim_id in claim_ids {
            let old_exists = transaction
                .query_row(
                    "SELECT 1 FROM resource_claim_bindings WHERE claim_id=?1 AND deployment_id=?2",
                    params![claim_id, old_deployment_id],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
                .is_some();
            if !old_exists {
                return Err(ResourceClaimError::Provider(format!(
                    "old deployment lost ResourceClaim binding {claim_id} before replacement commit"
                )));
            }
            transaction
                .execute(
                    "INSERT OR IGNORE INTO resource_claim_bindings (claim_id,deployment_id,created_at_ms) VALUES (?1,?2,?3)",
                    params![claim_id, new_deployment_id, crate::now_ms()],
                )
                .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        }
        transaction
            .commit()
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        Ok(())
    }

    fn purge(
        &self,
        payload: &orchestrator_runtime::ResourcePurgePayloadV1,
    ) -> Result<ResourceClaimV1> {
        payload
            .validate()
            .map_err(ResourceClaimError::InvalidPurgeAuditIntent)?;
        let state = self
            .state
            .lock()
            .map_err(|_| ResourceClaimError::Provider("claim state lock poisoned".to_string()))?;
        let json = state
            .query_row(
                "SELECT claim_json FROM resource_claim_state WHERE claim_id=?1",
                params![payload.claim_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?
            .ok_or_else(|| {
                ResourceClaimError::Provider(
                    "resource purge target is not durable on this Agent".to_string(),
                )
            })?;
        let claim: ResourceClaimV1 = serde_json::from_str(&json)
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        claim.validate()?;
        if claim.claim_digest != payload.claim_digest || claim.generation != payload.generation {
            return Err(ResourceClaimError::InvalidPurgeAuditIntent(
                "claim digest and generation do not match durable Agent state".to_string(),
            ));
        }
        if !matches!(
            claim.status,
            ResourceClaimStatusV1::Retained | ResourceClaimStatusV1::Purging
        ) {
            return Err(ResourceClaimError::ActionNotAllowed {
                action: ResourceClaimActionKindV1::Purge,
                status: claim.status,
            });
        }
        let bindings = state
            .query_row(
                "SELECT COUNT(*) FROM resource_claim_bindings WHERE claim_id=?1",
                params![payload.claim_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        if bindings != 0 {
            return Err(ResourceClaimError::Provider(
                "resource purge target still has a deployment binding".to_string(),
            ));
        }
        // Keep the Agent-local state mutex across provider execution. This is
        // the single-writer fence that prevents a concurrent replacement or
        // install from rebinding the claim between the binding check and DROP.
        let next = execute_resource_claim(
            claim,
            ResourceClaimActionV1::Purge {
                authorization: PurgeAuthorizationV1 {
                    confirmation: payload.confirmation.clone(),
                    audit_intent: PurgeAuditIntentV1 {
                        intent_id: payload.audit_intent.intent_id.clone(),
                        actor_id: payload.audit_intent.actor_id.clone(),
                        reason: payload.reason.clone(),
                        claim_digest: payload.audit_intent.claim_digest.clone(),
                        generation: payload.audit_intent.generation,
                    },
                },
            },
            &self.provider,
            &self.executor,
            &self.secrets,
            &self.random,
        )?;
        let next_json = serde_json::to_string(&next)
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        state
            .execute(
                "UPDATE resource_claim_state SET claim_json=?2, updated_at_ms=?3 WHERE claim_id=?1",
                params![payload.claim_id, next_json, crate::now_ms()],
            )
            .map_err(|error| ResourceClaimError::Provider(error.to_string()))?;
        Ok(next)
    }

    fn output_path(&self, reference: &str) -> Result<PathBuf> {
        self.secrets
            .output_path(reference)
            .map_err(ResourceClaimError::SecretStore)
    }
}

impl LivePostgreSqlExecutor {
    pub fn new(config: PostgreSqlAdminConfigV1) -> std::result::Result<Self, String> {
        config
            .provider
            .validate()
            .map_err(|error| error.to_string())?;
        validate_admin_connection(&config)?;
        if let Some(parent) = config
            .state_database
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create PostgreSQL receipt directory: {error}"))?;
        }
        let receipts = Connection::open(&config.state_database)
            .map_err(|error| format!("open PostgreSQL resource receipt ledger: {error}"))?;
        receipts
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("configure PostgreSQL resource receipt ledger: {error}"))?;
        receipts
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .and_then(|_| receipts.execute_batch(POSTGRES_COMMAND_TABLE))
            .map_err(|error| format!("initialize PostgreSQL resource receipt ledger: {error}"))?;
        Ok(Self {
            config,
            receipts: Mutex::new(receipts),
        })
    }

    fn connect(&self) -> std::result::Result<PostgreSqlClient, ProviderExecutionErrorV1> {
        let url = std::str::from_utf8(self.config.admin_url.expose_bytes())
            .map_err(|_| provider_rejected("admin-credential-is-not-utf8"))?;
        let mut config = url
            .parse::<PostgreSqlConfig>()
            .map_err(|_| provider_rejected("admin-connection-invalid"))?;
        config.application_name("ojos-agent-resource-provider");
        let tls = match &self.config.tls_trust {
            PostgreSqlTlsTrustV1::Platform => config_platform_verifier()
                .map_err(|_| provider_rejected("postgres-tls-platform-verifier"))?,
            PostgreSqlTlsTrustV1::CaCertificate(path) => config_from_ca_cert(path)
                .map_err(|_| provider_rejected("postgres-tls-ca-invalid"))?,
        };
        config
            .connect(MakeRustlsConnect::new(tls))
            .map_err(classify_postgres_error)
    }

    fn apply(
        &self,
        command: &PostgreSqlCommandV1,
        credential: Option<&SecretMaterial>,
    ) -> std::result::Result<PostgreSqlEvidenceV1, ProviderExecutionErrorV1> {
        if command.provider_id != self.config.provider.provider_id {
            return Err(provider_rejected("provider-id-mismatch"));
        }
        let mut client = self.connect()?;
        match command.action {
            ResourceClaimActionKindV1::Ensure => {
                let credential =
                    credential.ok_or_else(|| provider_rejected("credential-required"))?;
                if Some(credential.digest()) != command.credential_digest {
                    return Err(provider_rejected("credential-digest-mismatch"));
                }
                ensure_postgres_resource(&mut client, command, credential)?;
            }
            ResourceClaimActionKindV1::Inspect => {}
            ResourceClaimActionKindV1::Release => {
                let role = quote_identifier(&command.role_name);
                client
                    .batch_execute(&format!("ALTER ROLE {role} NOLOGIN"))
                    .map_err(classify_postgres_error)?;
            }
            ResourceClaimActionKindV1::Purge => {
                purge_postgres_resource(&mut client, command)?;
            }
        }
        inspect_postgres_resource(&mut client, command)
    }
}

impl PostgreSqlCommandExecutor for LivePostgreSqlExecutor {
    fn execute(
        &self,
        command: &PostgreSqlCommandV1,
        credential: Option<&SecretMaterial>,
    ) -> std::result::Result<PostgreSqlExecutionOutcomeV1, ProviderExecutionErrorV1> {
        command
            .validate()
            .map_err(|_| provider_rejected("invalid-command"))?;
        // Inspection is an observation, not a mutation. Replaying a completed
        // inspection receipt would make recovery permanently stale after an
        // operator repairs PostgreSQL, so always read the live catalog.
        if command.action == ResourceClaimActionKindV1::Inspect {
            return self
                .apply(command, credential)
                .map(|evidence| PostgreSqlExecutionOutcomeV1::Applied { evidence });
        }
        let mut receipts = self
            .receipts
            .lock()
            .map_err(|_| provider_fact_unknown("receipt-ledger-poisoned"))?;
        let transaction = receipts
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| provider_retryable("receipt-ledger-unavailable"))?;
        let existing = transaction
            .query_row(
                "SELECT request_digest, state, evidence_json FROM resource_claim_commands WHERE idempotency_key = ?1",
                params![command.idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
            )
            .optional()
            .map_err(|_| provider_retryable("receipt-ledger-read"))?;
        if let Some((request_digest, state, evidence_json)) = existing {
            if request_digest != command.request_digest {
                return Err(ProviderExecutionErrorV1 {
                    kind: ProviderFailureKindV1::IdempotencyConflict,
                    code: "same-key-different-request".to_string(),
                });
            }
            if state == "COMPLETED" && command.action != ResourceClaimActionKindV1::Ensure {
                let evidence = evidence_json
                    .ok_or_else(|| provider_fact_unknown("completed-receipt-without-evidence"))
                    .and_then(|json| {
                        serde_json::from_str(&json)
                            .map_err(|_| provider_fact_unknown("invalid-receipt-evidence"))
                    })?;
                transaction
                    .commit()
                    .map_err(|_| provider_retryable("receipt-ledger-commit"))?;
                return Ok(PostgreSqlExecutionOutcomeV1::Replayed { evidence });
            }
            // ENSURE receipts are evidence of a prior response, not a source
            // of current PostgreSQL truth. Re-run the idempotent catalog plan
            // so deletion/drift or a stale receipt cannot be mistaken for
            // READY after an Agent restart.
        } else {
            transaction
                .execute(
                    "INSERT INTO resource_claim_commands (idempotency_key, request_digest, state, updated_at_ms) VALUES (?1, ?2, 'STARTED', ?3)",
                    params![command.idempotency_key, command.request_digest, crate::now_ms()],
                )
                .map_err(|_| provider_retryable("receipt-ledger-insert"))?;
        }
        transaction
            .commit()
            .map_err(|_| provider_retryable("receipt-ledger-commit"))?;

        let evidence = self.apply(command, credential)?;
        let evidence_json = serde_json::to_string(&evidence)
            .map_err(|_| provider_fact_unknown("encode-evidence"))?;
        receipts
            .execute(
                "UPDATE resource_claim_commands SET state='COMPLETED', evidence_json=?2, updated_at_ms=?3 WHERE idempotency_key=?1 AND request_digest=?4",
                params![command.idempotency_key, evidence_json, crate::now_ms(), command.request_digest],
            )
            .map_err(|_| provider_fact_unknown("provider-applied-receipt-unknown"))?;
        Ok(PostgreSqlExecutionOutcomeV1::Applied { evidence })
    }
}

pub fn execute_resource_claim<E, S, G>(
    mut claim: ResourceClaimV1,
    action: ResourceClaimActionV1,
    provider: &PostgreSqlProviderDescriptorV1,
    executor: &E,
    secret_store: &S,
    password_generator: &G,
) -> Result<ResourceClaimV1>
where
    E: PostgreSqlCommandExecutor,
    S: ResourceSecretStore,
    G: CryptographicPasswordGenerator,
{
    claim.validate()?;
    provider.validate()?;
    if provider.provider_id != claim.provider_id {
        return Err(ResourceClaimError::InvalidProvider(
            "descriptor providerId does not match claim".to_string(),
        ));
    }
    validate_action_status(action.kind(), claim.status)?;
    match action {
        ResourceClaimActionV1::Ensure => {
            execute_ensure(claim, provider, executor, secret_store, password_generator)
        }
        ResourceClaimActionV1::Inspect => execute_inspect(claim, provider, executor, secret_store),
        ResourceClaimActionV1::Release => {
            transition(&mut claim, ResourceClaimStatusV1::Releasing)?;
            let command =
                plan_postgresql_command(&claim, ResourceClaimActionKindV1::Release, None, None)?;
            finish_release(claim, command, executor, secret_store)
        }
        ResourceClaimActionV1::Purge { authorization } => {
            validate_purge_authorization(&claim, &authorization)?;
            transition(&mut claim, ResourceClaimStatusV1::Purging)?;
            let audit_digest = authorization.audit_intent.digest()?;
            claim.purge_audit_intent_digest = Some(audit_digest.clone());
            let command = plan_postgresql_command(
                &claim,
                ResourceClaimActionKindV1::Purge,
                None,
                Some(audit_digest),
            )?;
            finish_purge(claim, command, executor, secret_store)
        }
    }
}

fn execute_ensure<E, S, G>(
    mut claim: ResourceClaimV1,
    provider: &PostgreSqlProviderDescriptorV1,
    executor: &E,
    secret_store: &S,
    password_generator: &G,
) -> Result<ResourceClaimV1>
where
    E: PostgreSqlCommandExecutor,
    S: ResourceSecretStore,
    G: CryptographicPasswordGenerator,
{
    transition(&mut claim, ResourceClaimStatusV1::Provisioning)?;
    claim.failure = None;
    let candidate = SecretMaterial::random_password(password_generator)?;
    let credential =
        match secret_store.get_or_create_0600(&claim.credential_secret_key(), candidate) {
            Ok(material) => material,
            Err(_) => {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::CredentialMaterialUnavailable,
                    true,
                    ResourceClaimStatusV1::Provisioning,
                );
            }
        };
    let command = plan_postgresql_command(
        &claim,
        ResourceClaimActionKindV1::Ensure,
        Some(credential.digest()),
        None,
    )?;
    match executor.execute(&command, Some(&credential)) {
        Ok(PostgreSqlExecutionOutcomeV1::Applied { evidence })
        | Ok(PostgreSqlExecutionOutcomeV1::Replayed { evidence }) => {
            verify_evidence_for_command(&command, &evidence)?;
            if !ready_evidence(&evidence, &credential.digest()) {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::ProviderEvidenceMismatch,
                    false,
                    ResourceClaimStatusV1::NeedsAttention,
                );
            }
            let output = materialize_dsn(&claim, provider, &credential, secret_store);
            let output = match output {
                Ok(output) => output,
                Err(_) => {
                    return terminal_failure(
                        claim,
                        ResourceClaimFailureCodeV1::SecretMaterializationFailed,
                        true,
                        ResourceClaimStatusV1::Provisioning,
                    );
                }
            };
            claim.evidence = Some(evidence);
            claim.output_secret = Some(output);
            transition(&mut claim, ResourceClaimStatusV1::Ready)?;
            claim.failure = None;
            claim.validate()?;
            Ok(claim)
        }
        Ok(PostgreSqlExecutionOutcomeV1::InProgress) => {
            claim.status = ResourceClaimStatusV1::Provisioning;
            claim.validate()?;
            Ok(claim)
        }
        Err(error) => provider_failure(claim, error, ResourceClaimStatusV1::Provisioning),
    }
}

fn execute_inspect<E, S>(
    claim: ResourceClaimV1,
    provider: &PostgreSqlProviderDescriptorV1,
    executor: &E,
    secret_store: &S,
) -> Result<ResourceClaimV1>
where
    E: PostgreSqlCommandExecutor,
    S: ResourceSecretStore,
{
    let command = plan_postgresql_command(&claim, ResourceClaimActionKindV1::Inspect, None, None)?;
    match executor.execute(&command, None) {
        Ok(PostgreSqlExecutionOutcomeV1::Applied { evidence })
        | Ok(PostgreSqlExecutionOutcomeV1::Replayed { evidence }) => {
            verify_evidence_for_command(&command, &evidence)?;
            reconcile_inspection(claim, provider, evidence, secret_store)
        }
        Ok(PostgreSqlExecutionOutcomeV1::InProgress) => Ok(claim),
        Err(error) => {
            let status = claim.status;
            provider_failure(claim, error, status)
        }
    }
}

fn finish_release<E, S>(
    mut claim: ResourceClaimV1,
    command: PostgreSqlCommandV1,
    executor: &E,
    secret_store: &S,
) -> Result<ResourceClaimV1>
where
    E: PostgreSqlCommandExecutor,
    S: ResourceSecretStore,
{
    match executor.execute(&command, None) {
        Ok(PostgreSqlExecutionOutcomeV1::Applied { evidence })
        | Ok(PostgreSqlExecutionOutcomeV1::Replayed { evidence }) => {
            verify_evidence_for_command(&command, &evidence)?;
            if !retained_evidence(&evidence) {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::ProviderEvidenceMismatch,
                    false,
                    ResourceClaimStatusV1::NeedsAttention,
                );
            }
            if let Some(output) = &claim.output_secret
                && secret_store.remove_output(&output.reference).is_err()
            {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::SecretMaterializationFailed,
                    true,
                    ResourceClaimStatusV1::Releasing,
                );
            }
            claim.output_secret = None;
            claim.evidence = Some(evidence);
            transition(&mut claim, ResourceClaimStatusV1::Retained)?;
            claim.failure = None;
            claim.validate()?;
            Ok(claim)
        }
        Ok(PostgreSqlExecutionOutcomeV1::InProgress) => Ok(claim),
        Err(error) => provider_failure(claim, error, ResourceClaimStatusV1::Releasing),
    }
}

fn finish_purge<E, S>(
    mut claim: ResourceClaimV1,
    command: PostgreSqlCommandV1,
    executor: &E,
    secret_store: &S,
) -> Result<ResourceClaimV1>
where
    E: PostgreSqlCommandExecutor,
    S: ResourceSecretStore,
{
    match executor.execute(&command, None) {
        Ok(PostgreSqlExecutionOutcomeV1::Applied { evidence })
        | Ok(PostgreSqlExecutionOutcomeV1::Replayed { evidence }) => {
            verify_evidence_for_command(&command, &evidence)?;
            if !deleted_evidence(&evidence)
                || evidence.purge_audit_intent_digest != command.purge_audit_intent_digest
            {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::ProviderEvidenceMismatch,
                    false,
                    ResourceClaimStatusV1::NeedsAttention,
                );
            }
            if let Some(output) = &claim.output_secret
                && secret_store.remove_output(&output.reference).is_err()
            {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::SecretMaterializationFailed,
                    true,
                    ResourceClaimStatusV1::Purging,
                );
            }
            if secret_store
                .remove_internal(&claim.credential_secret_key())
                .is_err()
            {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::SecretMaterializationFailed,
                    true,
                    ResourceClaimStatusV1::Purging,
                );
            }
            claim.output_secret = None;
            claim.evidence = Some(evidence);
            transition(&mut claim, ResourceClaimStatusV1::Deleted)?;
            claim.failure = None;
            claim.validate()?;
            Ok(claim)
        }
        Ok(PostgreSqlExecutionOutcomeV1::InProgress) => Ok(claim),
        Err(error) => provider_failure(claim, error, ResourceClaimStatusV1::Purging),
    }
}

fn reconcile_inspection<S: ResourceSecretStore>(
    mut claim: ResourceClaimV1,
    provider: &PostgreSqlProviderDescriptorV1,
    evidence: PostgreSqlEvidenceV1,
    secret_store: &S,
) -> Result<ResourceClaimV1> {
    match claim.status {
        ResourceClaimStatusV1::Provisioning
        | ResourceClaimStatusV1::Ready
        | ResourceClaimStatusV1::Requested
        | ResourceClaimStatusV1::Failed => {
            let credential = secret_store
                .read_0600(&claim.credential_secret_key())
                .map_err(ResourceClaimError::SecretStore)?;
            if let Some(credential) = credential
                && ready_evidence(&evidence, &credential.digest())
            {
                let output = materialize_dsn(&claim, provider, &credential, secret_store)?;
                claim.output_secret = Some(output);
                claim.evidence = Some(evidence);
                force_reconciled_status(&mut claim, ResourceClaimStatusV1::Ready)?;
                claim.failure = None;
                claim.validate()?;
                return Ok(claim);
            }
            claim.evidence = Some(evidence);
            terminal_failure(
                claim,
                ResourceClaimFailureCodeV1::ProviderFactUnknown,
                false,
                ResourceClaimStatusV1::NeedsAttention,
            )
        }
        ResourceClaimStatusV1::Releasing | ResourceClaimStatusV1::Retained => {
            if !retained_evidence(&evidence) {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::ProviderFactUnknown,
                    false,
                    ResourceClaimStatusV1::NeedsAttention,
                );
            }
            if let Some(output) = &claim.output_secret {
                secret_store
                    .remove_output(&output.reference)
                    .map_err(ResourceClaimError::SecretStore)?;
            }
            claim.output_secret = None;
            claim.evidence = Some(evidence);
            force_reconciled_status(&mut claim, ResourceClaimStatusV1::Retained)?;
            claim.failure = None;
            claim.validate()?;
            Ok(claim)
        }
        ResourceClaimStatusV1::Purging | ResourceClaimStatusV1::Deleted => {
            if !deleted_evidence(&evidence) {
                return terminal_failure(
                    claim,
                    ResourceClaimFailureCodeV1::ProviderFactUnknown,
                    false,
                    ResourceClaimStatusV1::NeedsAttention,
                );
            }
            claim.output_secret = None;
            claim.evidence = Some(evidence);
            force_reconciled_status(&mut claim, ResourceClaimStatusV1::Deleted)?;
            claim.failure = None;
            claim.validate()?;
            Ok(claim)
        }
        ResourceClaimStatusV1::NeedsAttention => {
            let credential = secret_store
                .read_0600(&claim.credential_secret_key())
                .map_err(ResourceClaimError::SecretStore)?;
            if let Some(credential) = credential
                && ready_evidence(&evidence, &credential.digest())
            {
                let output = materialize_dsn(&claim, provider, &credential, secret_store)?;
                claim.output_secret = Some(output);
                claim.evidence = Some(evidence);
                force_reconciled_status(&mut claim, ResourceClaimStatusV1::Ready)?;
                claim.failure = None;
                claim.validate()?;
                return Ok(claim);
            }
            claim.evidence = Some(evidence);
            Ok(claim)
        }
    }
}

fn materialize_dsn<S: ResourceSecretStore>(
    claim: &ResourceClaimV1,
    provider: &PostgreSqlProviderDescriptorV1,
    credential: &SecretMaterial,
    secret_store: &S,
) -> Result<ResourceOutputSecretV1> {
    let names = claim.postgres_names()?;
    let credential = std::str::from_utf8(credential.expose_bytes()).map_err(|_| {
        ResourceClaimError::SecretStore("generated credential must be UTF-8".to_string())
    })?;
    let host = dsn_host(&provider.host);
    let dsn = format!(
        "postgresql://{}:{}@{}:{}/{}?sslmode={}",
        names.role_name,
        credential,
        host,
        provider.port,
        names.database_name,
        provider.sslmode()
    );
    let reference = claim.output_secret_reference();
    let write = secret_store
        .write_output_0600(&reference, SecretMaterial::new(dsn.into_bytes())?)
        .map_err(ResourceClaimError::SecretStore)?;
    if write.reference != reference || write.mode != OUTPUT_SECRET_MODE {
        return Err(ResourceClaimError::InvalidSecretEvidence(
            "secret sink returned the wrong reference or mode".to_string(),
        ));
    }
    validate_digest("secretWrite.contentDigest", &write.content_digest)?;
    Ok(ResourceOutputSecretV1 {
        reference: write.reference,
        content_digest: write.content_digest,
        mode: write.mode,
        generation: claim.generation,
    })
}

fn verify_evidence_for_command(
    command: &PostgreSqlCommandV1,
    evidence: &PostgreSqlEvidenceV1,
) -> Result<()> {
    command.validate()?;
    evidence.validate()?;
    if evidence.claim_digest != command.claim_digest
        || evidence.generation != command.generation
        || evidence.provider_id != command.provider_id
        || evidence.database_name != command.database_name
        || evidence.role_name != command.role_name
    {
        return Err(ResourceClaimError::InvalidEvidence(
            "identity does not match command".to_string(),
        ));
    }
    if command.action == ResourceClaimActionKindV1::Ensure
        && evidence.credential_digest != command.credential_digest
    {
        return Err(ResourceClaimError::InvalidEvidence(
            "credential digest does not match ENSURE command".to_string(),
        ));
    }
    Ok(())
}

fn ready_evidence(evidence: &PostgreSqlEvidenceV1, credential_digest: &str) -> bool {
    evidence.database_exists
        && evidence.role_exists
        && evidence.owner_matches
        && evidence.role_can_login
        && evidence.credential_digest.as_deref() == Some(credential_digest)
}

fn retained_evidence(evidence: &PostgreSqlEvidenceV1) -> bool {
    evidence.database_exists
        && evidence.role_exists
        && evidence.owner_matches
        && !evidence.role_can_login
}

fn deleted_evidence(evidence: &PostgreSqlEvidenceV1) -> bool {
    !evidence.database_exists && !evidence.role_exists
}

fn provider_failure(
    claim: ResourceClaimV1,
    error: ProviderExecutionErrorV1,
    retry_status: ResourceClaimStatusV1,
) -> Result<ResourceClaimV1> {
    match error.kind {
        ProviderFailureKindV1::Retryable => terminal_failure(
            claim,
            ResourceClaimFailureCodeV1::ProviderUnavailable,
            true,
            retry_status,
        ),
        ProviderFailureKindV1::Rejected => terminal_failure(
            claim,
            ResourceClaimFailureCodeV1::ProviderRejected,
            false,
            ResourceClaimStatusV1::Failed,
        ),
        ProviderFailureKindV1::FactUnknown => terminal_failure(
            claim,
            ResourceClaimFailureCodeV1::ProviderFactUnknown,
            false,
            ResourceClaimStatusV1::NeedsAttention,
        ),
        ProviderFailureKindV1::IdempotencyConflict => terminal_failure(
            claim,
            ResourceClaimFailureCodeV1::IdempotencyConflict,
            false,
            ResourceClaimStatusV1::NeedsAttention,
        ),
    }
}

fn terminal_failure(
    mut claim: ResourceClaimV1,
    code: ResourceClaimFailureCodeV1,
    retryable: bool,
    status: ResourceClaimStatusV1,
) -> Result<ResourceClaimV1> {
    if claim.status != status {
        transition(&mut claim, status)?;
    }
    claim.failure = Some(ResourceClaimFailureV1 { code, retryable });
    claim.validate()?;
    Ok(claim)
}

pub fn transition(claim: &mut ResourceClaimV1, next: ResourceClaimStatusV1) -> Result<()> {
    if claim.status == next {
        return Ok(());
    }
    let valid = matches!(
        (claim.status, next),
        (
            ResourceClaimStatusV1::Requested,
            ResourceClaimStatusV1::Provisioning | ResourceClaimStatusV1::NeedsAttention
        ) | (
            ResourceClaimStatusV1::Provisioning,
            ResourceClaimStatusV1::Ready
                | ResourceClaimStatusV1::Failed
                | ResourceClaimStatusV1::NeedsAttention
        ) | (
            ResourceClaimStatusV1::Ready,
            ResourceClaimStatusV1::Provisioning
                | ResourceClaimStatusV1::Releasing
                | ResourceClaimStatusV1::NeedsAttention
        ) | (
            ResourceClaimStatusV1::Releasing,
            ResourceClaimStatusV1::Retained
                | ResourceClaimStatusV1::Failed
                | ResourceClaimStatusV1::NeedsAttention
        ) | (
            ResourceClaimStatusV1::Retained,
            ResourceClaimStatusV1::Provisioning
                | ResourceClaimStatusV1::Purging
                | ResourceClaimStatusV1::NeedsAttention
        ) | (
            ResourceClaimStatusV1::Purging,
            ResourceClaimStatusV1::Deleted
                | ResourceClaimStatusV1::Failed
                | ResourceClaimStatusV1::NeedsAttention
        ) | (
            ResourceClaimStatusV1::Failed,
            ResourceClaimStatusV1::Provisioning
                | ResourceClaimStatusV1::NeedsAttention
                | ResourceClaimStatusV1::Failed
        ) | (
            ResourceClaimStatusV1::NeedsAttention,
            ResourceClaimStatusV1::Provisioning
                | ResourceClaimStatusV1::Releasing
                | ResourceClaimStatusV1::Purging
                | ResourceClaimStatusV1::Ready
                | ResourceClaimStatusV1::Retained
                | ResourceClaimStatusV1::Deleted
        )
    );
    if !valid {
        return Err(ResourceClaimError::InvalidStatusTransition {
            from: claim.status,
            to: next,
        });
    }
    claim.status = next;
    Ok(())
}

fn force_reconciled_status(claim: &mut ResourceClaimV1, next: ResourceClaimStatusV1) -> Result<()> {
    if transition(claim, next).is_err() {
        transition(claim, ResourceClaimStatusV1::NeedsAttention)?;
        transition(claim, next)?;
    }
    Ok(())
}

fn validate_action_status(
    action: ResourceClaimActionKindV1,
    status: ResourceClaimStatusV1,
) -> Result<()> {
    let allowed = match action {
        ResourceClaimActionKindV1::Ensure => matches!(
            status,
            ResourceClaimStatusV1::Requested
                | ResourceClaimStatusV1::Provisioning
                | ResourceClaimStatusV1::Ready
                | ResourceClaimStatusV1::Retained
                | ResourceClaimStatusV1::Failed
        ),
        ResourceClaimActionKindV1::Inspect => true,
        ResourceClaimActionKindV1::Release => matches!(
            status,
            ResourceClaimStatusV1::Ready | ResourceClaimStatusV1::Releasing
        ),
        ResourceClaimActionKindV1::Purge => matches!(
            status,
            ResourceClaimStatusV1::Retained | ResourceClaimStatusV1::Purging
        ),
    };
    if !allowed {
        return Err(ResourceClaimError::ActionNotAllowed { action, status });
    }
    Ok(())
}

fn validate_purge_authorization(
    claim: &ResourceClaimV1,
    authorization: &PurgeAuthorizationV1,
) -> Result<()> {
    let expected = claim.purge_confirmation();
    if authorization.confirmation != expected {
        return Err(ResourceClaimError::PurgeConfirmationMismatch { expected });
    }
    authorization.audit_intent.validate_for(claim)
}

fn validate_identity(identity: &ResourceClaimIdentityV1) -> Result<()> {
    validate_identifier("identity.claimId", &identity.claim_id)?;
    validate_identifier("identity.ownerInstanceId", &identity.owner_instance_id)?;
    validate_identifier("identity.serviceId", &identity.service_id)?;
    validate_identifier("identity.resourceName", &identity.resource_name)?;
    if identity.resource_type != RESOURCE_TYPE_POSTGRESQL_DATABASE {
        return Err(ResourceClaimError::UnsupportedResourceType(
            identity.resource_type.clone(),
        ));
    }
    Ok(())
}

fn validate_evidence_identity(
    claim: &ResourceClaimV1,
    evidence: &PostgreSqlEvidenceV1,
) -> Result<()> {
    evidence.validate()?;
    let names = postgres_names(&claim.claim_digest)?;
    if evidence.claim_digest != claim.claim_digest
        || evidence.generation != claim.generation
        || evidence.provider_id != claim.provider_id
        || evidence.database_name != names.database_name
        || evidence.role_name != names.role_name
    {
        return Err(ResourceClaimError::InvalidEvidence(
            "claim evidence identity mismatch".to_string(),
        ));
    }
    Ok(())
}

fn idempotency_key(
    claim_digest: &str,
    generation: u64,
    action: ResourceClaimActionKindV1,
) -> String {
    let action = match action {
        ResourceClaimActionKindV1::Ensure => "ensure",
        ResourceClaimActionKindV1::Inspect => "inspect",
        ResourceClaimActionKindV1::Release => "release",
        ResourceClaimActionKindV1::Purge => "purge",
    };
    format!(
        "resource:{}:g{generation}:{action}",
        digest_hex(claim_digest)
    )
}

fn command_request_digest(command: &PostgreSqlCommandV1) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"ojos.dev/postgresql-command/v1");
    for value in [
        format!("{:?}", command.action),
        command.claim_digest.clone(),
        command.generation.to_string(),
        command.provider_id.clone(),
        command.database_name.clone(),
        command.role_name.clone(),
        command.idempotency_key.clone(),
        command.credential_digest.clone().unwrap_or_default(),
        command
            .purge_audit_intent_digest
            .clone()
            .unwrap_or_default(),
        command
            .steps
            .iter()
            .map(|step| format!("{step:?}"))
            .collect::<Vec<_>>()
            .join(","),
    ] {
        hash_field(&mut hasher, value.as_bytes());
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn evidence_digest(evidence: &PostgreSqlEvidenceV1) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"ojos.dev/postgresql-evidence/v1");
    for value in [
        evidence.claim_digest.clone(),
        evidence.generation.to_string(),
        evidence.provider_id.clone(),
        evidence.database_name.clone(),
        evidence.role_name.clone(),
        evidence.database_exists.to_string(),
        evidence.role_exists.to_string(),
        evidence.owner_matches.to_string(),
        evidence.role_can_login.to_string(),
        evidence.credential_digest.clone().unwrap_or_default(),
        evidence
            .purge_audit_intent_digest
            .clone()
            .unwrap_or_default(),
    ] {
        hash_field(&mut hasher, value.as_bytes());
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn validate_identifier(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 180
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
    {
        return Err(ResourceClaimError::InvalidIdentifier {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_actor_id(value: &str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        return Err(ResourceClaimError::InvalidPurgeAuditIntent(
            "actorId must be a non-empty authenticated subject without whitespace padding or controls"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<()> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if !valid {
        return Err(ResourceClaimError::InvalidDigest {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_postgres_name(field: &'static str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 63
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ResourceClaimError::InvalidProvider(format!(
            "{field} must be a safe lowercase PostgreSQL identifier"
        )));
    }
    Ok(())
}

fn validate_secret_reference(value: &str) -> Result<()> {
    if !value.starts_with("agent-secret://")
        || value.len() > 1024
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(ResourceClaimError::InvalidSecretEvidence(
            "output must be an agent-secret:// reference".to_string(),
        ));
    }
    Ok(())
}

fn valid_host(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 253
        || value.contains("//")
        || value.contains('@')
        || value.contains('/')
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return false;
    }
    if value.parse::<IpAddr>().is_ok() {
        return true;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn dsn_host(host: &str) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn ensure_postgres_resource(
    client: &mut PostgreSqlClient,
    command: &PostgreSqlCommandV1,
    credential: &SecretMaterial,
) -> std::result::Result<(), ProviderExecutionErrorV1> {
    let credential = std::str::from_utf8(credential.expose_bytes())
        .map_err(|_| provider_rejected("credential-is-not-utf8"))?;
    let verifier = scram_sha_256(credential.as_bytes());
    let role = quote_identifier(&command.role_name);
    let database = quote_identifier(&command.database_name);
    let role_exists = client
        .query_opt(
            "SELECT 1 FROM pg_roles WHERE rolname = $1",
            &[&command.role_name],
        )
        .map_err(classify_postgres_error)?
        .is_some();
    // PostgreSQL utility statements do not accept extended-query parameters.
    // Embed only the client-generated SCRAM verifier (never the plaintext
    // password) and quote it as a SQL literal.
    let verifier = quote_sql_literal(&verifier);
    if role_exists {
        client
            .batch_execute(&format!("ALTER ROLE {role} LOGIN PASSWORD {verifier}"))
            .map_err(classify_postgres_error)?;
    } else {
        client
            .batch_execute(&format!("CREATE ROLE {role} LOGIN PASSWORD {verifier}"))
            .map_err(classify_postgres_error)?;
    }
    let database_owner = client
        .query_opt(
            "SELECT owner.rolname FROM pg_database db JOIN pg_roles owner ON owner.oid = db.datdba WHERE db.datname = $1",
            &[&command.database_name],
        )
        .map_err(classify_postgres_error)?
        .map(|row| row.get::<_, String>(0));
    match database_owner {
        None => client
            .batch_execute(&format!("CREATE DATABASE {database} OWNER {role}"))
            .map_err(classify_postgres_error)?,
        Some(owner) if owner == command.role_name => {}
        Some(_) => return Err(provider_rejected("database-owner-conflict")),
    }
    client
        .batch_execute(&format!(
            "REVOKE ALL ON DATABASE {database} FROM PUBLIC; GRANT CONNECT, TEMPORARY ON DATABASE {database} TO {role}"
        ))
        .map_err(classify_postgres_error)?;
    Ok(())
}

fn purge_postgres_resource(
    client: &mut PostgreSqlClient,
    command: &PostgreSqlCommandV1,
) -> std::result::Result<(), ProviderExecutionErrorV1> {
    let before = inspect_postgres_resource(client, command)?;
    if !before.database_exists && !before.role_exists {
        return Ok(());
    }
    // A missing database with the exact generated role can only require the
    // final idempotent DROP ROLE after an interrupted purge. Every other
    // partial/mismatched catalog state is ambiguous and must be reconciled by
    // an operator rather than guessed from names.
    if before.database_exists && (!before.role_exists || !before.owner_matches) {
        return Err(provider_fact_unknown("purge-precondition-catalog-mismatch"));
    }
    let role = quote_identifier(&command.role_name);
    if before.role_exists {
        client
            .batch_execute(&format!("ALTER ROLE {role} NOLOGIN"))
            .map_err(classify_purge_mutation_error)?;
    }
    if before.database_exists {
        let database = quote_identifier(&command.database_name);
        client
            .batch_execute(&format!(
                "REVOKE CONNECT ON DATABASE {database} FROM PUBLIC; REVOKE CONNECT ON DATABASE {database} FROM {role}; ALTER DATABASE {database} ALLOW_CONNECTIONS false"
            ))
            .map_err(classify_purge_mutation_error)?;
        let terminations = client
            .query(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()",
                &[&command.database_name],
            )
            .map_err(classify_purge_mutation_error)?;
        if terminations.iter().any(|row| !row.get::<_, bool>(0)) {
            return Err(provider_fact_unknown(
                "purge-session-termination-unconfirmed",
            ));
        }
        client
            .batch_execute(&format!("DROP DATABASE {database}"))
            .map_err(classify_purge_mutation_error)?;
    }
    // Re-inspect before DROP ROLE. If the database still exists, never guess
    // that the preceding utility command committed.
    let after_database = inspect_postgres_resource(client, command)?;
    if after_database.database_exists {
        return Err(provider_fact_unknown("purge-database-drop-unconfirmed"));
    }
    if after_database.role_exists {
        client
            .batch_execute(&format!("DROP ROLE {role}"))
            .map_err(classify_purge_mutation_error)?;
    }
    let after = inspect_postgres_resource(client, command)?;
    if after.database_exists || after.role_exists {
        return Err(provider_fact_unknown("purge-final-state-unconfirmed"));
    }
    Ok(())
}

fn validate_admin_connection(config: &PostgreSqlAdminConfigV1) -> std::result::Result<(), String> {
    let url = std::str::from_utf8(config.admin_url.expose_bytes())
        .map_err(|_| "PostgreSQL administrator URL is not UTF-8".to_string())?;
    let parsed = url
        .parse::<PostgreSqlConfig>()
        .map_err(|_| "PostgreSQL administrator URL is invalid".to_string())?;
    if parsed.get_user().is_none_or(|user| user.trim().is_empty())
        || parsed
            .get_password()
            .is_none_or(|password| password.is_empty())
        || parsed
            .get_dbname()
            .is_none_or(|database| database.trim().is_empty())
        || parsed.get_ssl_mode() != PostgreSqlSslMode::Require
        || parsed.get_hosts().len() != 1
        || !matches!(&parsed.get_hosts()[0], PostgreSqlHost::Tcp(host) if host == &config.provider.host)
        || parsed.get_ports().first().copied().unwrap_or(5432) != config.provider.port
    {
        return Err(
            "PostgreSQL administrator URL must contain an explicit user, password, database, matching TCP host/port, and sslmode=require"
                .to_string(),
        );
    }
    if matches!(
        config.provider.tls_mode,
        PostgreSqlTlsModeV1::VerifyCa | PostgreSqlTlsModeV1::VerifyFull
    ) && !matches!(config.tls_trust, PostgreSqlTlsTrustV1::CaCertificate(ref path) if path.is_file())
    {
        return Err("PostgreSQL verify-ca/verify-full requires an existing CA file".to_string());
    }
    Ok(())
}

fn inspect_postgres_resource(
    client: &mut PostgreSqlClient,
    command: &PostgreSqlCommandV1,
) -> std::result::Result<PostgreSqlEvidenceV1, ProviderExecutionErrorV1> {
    let role = client
        .query_opt(
            "SELECT rolcanlogin FROM pg_roles WHERE rolname = $1",
            &[&command.role_name],
        )
        .map_err(classify_postgres_error)?;
    let database_owner = client
        .query_opt(
            "SELECT owner.rolname FROM pg_database db JOIN pg_roles owner ON owner.oid = db.datdba WHERE db.datname = $1",
            &[&command.database_name],
        )
        .map_err(classify_postgres_error)?
        .map(|row| row.get::<_, String>(0));
    PostgreSqlEvidenceV1 {
        claim_digest: command.claim_digest.clone(),
        generation: command.generation,
        provider_id: command.provider_id.clone(),
        database_name: command.database_name.clone(),
        role_name: command.role_name.clone(),
        database_exists: database_owner.is_some(),
        role_exists: role.is_some(),
        owner_matches: database_owner.as_deref() == Some(command.role_name.as_str()),
        role_can_login: role.map(|row| row.get::<_, bool>(0)).unwrap_or(false),
        credential_digest: if command.action == ResourceClaimActionKindV1::Ensure {
            command.credential_digest.clone()
        } else {
            None
        },
        purge_audit_intent_digest: command.purge_audit_intent_digest.clone(),
        evidence_digest: String::new(),
    }
    .seal()
    .map_err(|_| provider_fact_unknown("seal-provider-evidence"))
}

fn quote_identifier(value: &str) -> String {
    debug_assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    );
    format!("\"{value}\"")
}

fn quote_sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn classify_postgres_error(error: postgres::Error) -> ProviderExecutionErrorV1 {
    if let Some(database) = error.as_db_error() {
        let code = database.code().code();
        let kind = match code {
            "42501" | "42601" | "42710" | "55006" => ProviderFailureKindV1::Rejected,
            "40001" | "40P01" | "53300" | "57P01" | "57P02" | "57P03" => {
                ProviderFailureKindV1::Retryable
            }
            _ => ProviderFailureKindV1::FactUnknown,
        };
        return ProviderExecutionErrorV1 {
            kind,
            code: format!("postgres-sqlstate-{code}"),
        };
    }
    provider_retryable("postgres-connection")
}

fn classify_purge_mutation_error(error: postgres::Error) -> ProviderExecutionErrorV1 {
    let classified = classify_postgres_error(error);
    match classified.kind {
        // Once the first destructive request has been sent, transport and
        // server failures cannot prove how much of the sequence committed.
        ProviderFailureKindV1::Retryable | ProviderFailureKindV1::FactUnknown => {
            provider_fact_unknown("purge-provider-outcome-unknown")
        }
        ProviderFailureKindV1::Rejected | ProviderFailureKindV1::IdempotencyConflict => classified,
    }
}

fn provider_retryable(code: &str) -> ProviderExecutionErrorV1 {
    ProviderExecutionErrorV1 {
        kind: ProviderFailureKindV1::Retryable,
        code: code.to_string(),
    }
}

fn provider_rejected(code: &str) -> ProviderExecutionErrorV1 {
    ProviderExecutionErrorV1 {
        kind: ProviderFailureKindV1::Rejected,
        code: code.to_string(),
    }
}

fn provider_fact_unknown(code: &str) -> ProviderExecutionErrorV1 {
    ProviderExecutionErrorV1 {
        kind: ProviderFailureKindV1::FactUnknown,
        code: code.to_string(),
    }
}

fn validate_secret_key(key: &str) -> std::result::Result<(), String> {
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
        return Err("invalid resource secret key".to_string());
    }
    Ok(())
}

fn secret_component(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn output_reference_component(reference: &str) -> std::result::Result<String, String> {
    let remainder = reference
        .strip_prefix("agent-secret://resource-outputs/")
        .ok_or_else(|| "resource output reference has the wrong scheme".to_string())?;
    if remainder.is_empty()
        || remainder.len() > 256
        || Path::new(remainder)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("resource output reference is invalid".to_string());
    }
    Ok(secret_component(reference))
}

fn validate_isolated_secret_roots(
    internal_root: &Path,
    output_root: &Path,
) -> std::result::Result<(), String> {
    let internal = canonicalize_secret_root("internal resource state root", internal_root)?;
    let output = canonicalize_secret_root("resource output export root", output_root)?;
    if internal == output || internal.starts_with(&output) || output.starts_with(&internal) {
        return Err(
            "internal resource state and workload output export roots must not overlap".to_string(),
        );
    }
    Ok(())
}

fn canonicalize_secret_root(name: &str, path: &Path) -> std::result::Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("{name} must be an absolute normalized path"));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{name} must not be a symlink"))
        }
        Ok(metadata) if !metadata.is_dir() => Err(format!("{name} must be a directory")),
        Ok(_) => {
            fs::canonicalize(path).map_err(|error| format!("cannot canonicalize {name}: {error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            canonicalize_missing_secret_root(name, path)
        }
        Err(error) => Err(format!("cannot inspect {name}: {error}")),
    }
}

fn canonicalize_missing_secret_root(
    name: &str,
    path: &Path,
) -> std::result::Result<PathBuf, String> {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "existing ancestor of {name} is not a real directory"
                    ));
                }
                let mut canonical = fs::canonicalize(ancestor)
                    .map_err(|error| format!("cannot canonicalize ancestor of {name}: {error}"))?;
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor
                    .file_name()
                    .ok_or_else(|| format!("{name} has no existing ancestor"))?;
                missing.push(component.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| format!("{name} has no existing ancestor"))?;
            }
            Err(error) => return Err(format!("cannot inspect ancestor of {name}: {error}")),
        }
    }
}

fn create_private_directory(
    path: &Path,
    ownership: WorkloadFileOwnership,
) -> std::result::Result<(), String> {
    validate_workload_file_ownership(ownership)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("private secret directory must not be a symlink".to_string());
    }
    fs::create_dir_all(path)
        .map_err(|error| format!("create private secret directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("set private secret directory permissions: {error}"))?;
        let directory = File::open(path)
            .map_err(|error| format!("inspect private secret directory: {error}"))?;
        verify_unix_file_ownership(&directory, path, ownership, 0o700)?;
    }
    Ok(())
}

fn open_private_lock(
    path: &Path,
    _ownership: WorkloadFileOwnership,
) -> std::result::Result<File, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("open private secret lock: {error}"))?;
        verify_unix_file_ownership(&lock, path, _ownership, 0o600)?;
        return Ok(lock);
    }
    #[cfg(not(unix))]
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("open private secret lock: {error}"))
}

fn write_private_new(
    path: &Path,
    bytes: &[u8],
    ownership: WorkloadFileOwnership,
) -> std::result::Result<(), String> {
    validate_workload_file_ownership(ownership)?;
    if path.parent().is_none() {
        return Err("private secret path has no parent directory".to_string());
    }
    #[cfg(unix)]
    let parent = path.parent().expect("private secret parent was checked");
    let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), crate::now_ms()));
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary);
    let mut file = file.map_err(|error| format!("create immutable private secret: {error}"))?;
    let result: std::result::Result<(), String> = (|| {
        #[cfg(unix)]
        verify_unix_file_ownership(&file, &temporary, ownership, 0o600)?;
        file.write_all(bytes)
            .map_err(|error| format!("write private secret: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync private secret: {error}"))?;
        drop(file);
        // Linking a fully synced inode publishes it atomically and fails if
        // the destination exists. Unlike rename on Unix, this cannot replace
        // an immutable output in a race.
        fs::hard_link(&temporary, path).map_err(|error| {
            format!("publish private secret atomically without overwrite: {error}")
        })?;
        #[cfg(unix)]
        verify_unix_file_ownership(
            &File::open(path)
                .map_err(|error| format!("inspect published private secret: {error}"))?,
            path,
            ownership,
            0o600,
        )?;
        fs::remove_file(&temporary)
            .map_err(|error| format!("remove private secret staging link: {error}"))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync private secret directory: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(())
}

fn validate_workload_file_ownership(
    ownership: WorkloadFileOwnership,
) -> std::result::Result<(), String> {
    #[cfg(unix)]
    {
        if let Some((uid, gid)) = ownership.unix_ids() {
            // SAFETY: geteuid/getegid take no pointers and have no preconditions.
            let (effective_uid, effective_gid) = unsafe { (libc::geteuid(), libc::getegid()) };
            if effective_uid != uid || effective_gid != gid {
                return Err(format!(
                    "resource outputs require Agent effective identity {uid}:{gid}, observed {effective_uid}:{effective_gid}; refusing CAP_CHOWN fallback"
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if ownership.unix_ids().is_some() {
            return Err(
                "explicit Unix workload ownership is unsupported on this platform".to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(unix)]
fn verify_unix_file_ownership(
    file: &File,
    path: &Path,
    ownership: WorkloadFileOwnership,
    expected_mode: u32,
) -> std::result::Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect workload-owned path {}: {error}", path.display()))?;
    let expected = match ownership.unix_ids() {
        Some(ids) => ids,
        None => {
            // SAFETY: geteuid/getegid take no pointers and have no preconditions.
            unsafe { (libc::geteuid(), libc::getegid()) }
        }
    };
    if (metadata.uid(), metadata.gid()) != expected {
        return Err(format!(
            "workload-owned path {} has owner {}:{}, expected {}:{}",
            path.display(),
            metadata.uid(),
            metadata.gid(),
            expected.0,
            expected.1
        ));
    }
    let actual_mode = metadata.permissions().mode() & 0o777;
    if actual_mode != expected_mode {
        return Err(format!(
            "workload-owned path {} has mode {actual_mode:04o}, expected {expected_mode:04o}",
            path.display()
        ));
    }
    Ok(())
}

fn read_private_optional(
    path: &Path,
    _ownership: WorkloadFileOwnership,
) -> std::result::Result<Option<Vec<u8>>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("open private secret: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect private secret: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
        return Err("private secret is empty, oversized, or not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != OUTPUT_SECRET_MODE {
            return Err("private secret mode is not 0600".to_string());
        }
        verify_unix_file_ownership(&file, path, _ownership, OUTPUT_SECRET_MODE)?;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("read private secret: {error}"))?;
    Ok(Some(bytes))
}

fn remove_private_file(path: &Path) -> std::result::Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove private secret: {error}")),
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn digest_hex(value: &str) -> &str {
    value.strip_prefix("sha256:").unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Barrier, Mutex},
        thread,
    };

    fn identity() -> ResourceClaimIdentityV1 {
        ResourceClaimIdentityV1 {
            claim_id: "claim-contest-database".to_string(),
            owner_instance_id: "service-instance-contest".to_string(),
            service_id: "contest-service".to_string(),
            resource_name: "database".to_string(),
            resource_type: RESOURCE_TYPE_POSTGRESQL_DATABASE.to_string(),
        }
    }

    fn claim() -> ResourceClaimV1 {
        ResourceClaimV1::requested(identity(), 1, "postgresql-local").unwrap()
    }

    fn provider() -> PostgreSqlProviderDescriptorV1 {
        PostgreSqlProviderDescriptorV1 {
            provider_id: "postgresql-local".to_string(),
            host: "postgres.internal".to_string(),
            port: 5432,
            tls_mode: PostgreSqlTlsModeV1::VerifyFull,
        }
    }

    fn runtime_step(deployment_id: &str) -> orchestrator_runtime::ResourceClaimStepV1 {
        orchestrator_runtime::ResourceClaimStepV1 {
            claim_id: "claim-contest-database".to_string(),
            owner_instance_id: "service-instance-contest".to_string(),
            deployment_id: deployment_id.to_string(),
            service_id: "contest-service".to_string(),
            resource_name: "database".to_string(),
            resource_type: RESOURCE_TYPE_POSTGRESQL_DATABASE.to_string(),
            generation: 1,
            provider_id: "postgresql-local".to_string(),
            output_path_environment: "OJOS_RESOURCE_DATABASE_OUTPUT_FILE".to_string(),
        }
    }

    #[derive(Default)]
    struct FakeRandom {
        calls: Mutex<u8>,
    }

    impl CryptographicPasswordGenerator for FakeRandom {
        fn fill_random(&self, destination: &mut [u8]) -> std::result::Result<(), String> {
            let mut calls = self.calls.lock().unwrap();
            *calls = calls.wrapping_add(1);
            destination.fill(*calls);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeSecrets {
        internal: Mutex<BTreeMap<String, SecretMaterial>>,
        outputs: Mutex<BTreeMap<String, SecretMaterial>>,
        fail_output_once: Mutex<bool>,
    }

    impl FakeSecrets {
        fn with_output_crash() -> Self {
            Self {
                fail_output_once: Mutex::new(true),
                ..Self::default()
            }
        }

        fn output(&self, reference: &str) -> Option<SecretMaterial> {
            self.outputs.lock().unwrap().get(reference).cloned()
        }
    }

    impl ResourceSecretStore for FakeSecrets {
        fn get_or_create_0600(
            &self,
            key: &str,
            candidate: SecretMaterial,
        ) -> std::result::Result<SecretMaterial, String> {
            let mut values = self.internal.lock().unwrap();
            Ok(values.entry(key.to_string()).or_insert(candidate).clone())
        }

        fn read_0600(&self, key: &str) -> std::result::Result<Option<SecretMaterial>, String> {
            Ok(self.internal.lock().unwrap().get(key).cloned())
        }

        fn write_output_0600(
            &self,
            reference: &str,
            material: SecretMaterial,
        ) -> std::result::Result<SecretWriteEvidenceV1, String> {
            let mut fail = self.fail_output_once.lock().unwrap();
            if *fail {
                *fail = false;
                return Err("simulated crash after provider execution".to_string());
            }
            let digest = material.digest();
            self.outputs
                .lock()
                .unwrap()
                .insert(reference.to_string(), material);
            Ok(SecretWriteEvidenceV1 {
                reference: reference.to_string(),
                content_digest: digest,
                mode: OUTPUT_SECRET_MODE,
            })
        }

        fn remove_output(&self, reference: &str) -> std::result::Result<(), String> {
            self.outputs.lock().unwrap().remove(reference);
            Ok(())
        }

        fn remove_internal(&self, key: &str) -> std::result::Result<(), String> {
            self.internal.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeExecutor {
        completed: Mutex<BTreeMap<String, (String, PostgreSqlEvidenceV1)>>,
        facts: Mutex<Option<PostgreSqlEvidenceV1>>,
        applied: Mutex<BTreeMap<ResourceClaimActionKindV1, usize>>,
        apply_then_retry_once: Mutex<bool>,
    }

    impl FakeExecutor {
        fn with_apply_then_retry() -> Self {
            Self {
                apply_then_retry_once: Mutex::new(true),
                ..Self::default()
            }
        }

        fn apply_count(&self, action: ResourceClaimActionKindV1) -> usize {
            *self.applied.lock().unwrap().get(&action).unwrap_or(&0)
        }

        fn evidence_for(command: &PostgreSqlCommandV1) -> PostgreSqlEvidenceV1 {
            let (database_exists, role_exists, owner_matches, role_can_login) = match command.action
            {
                ResourceClaimActionKindV1::Ensure => (true, true, true, true),
                ResourceClaimActionKindV1::Release => (true, true, true, false),
                ResourceClaimActionKindV1::Purge => (false, false, false, false),
                ResourceClaimActionKindV1::Inspect => (false, false, false, false),
            };
            PostgreSqlEvidenceV1 {
                claim_digest: command.claim_digest.clone(),
                generation: command.generation,
                provider_id: command.provider_id.clone(),
                database_name: command.database_name.clone(),
                role_name: command.role_name.clone(),
                database_exists,
                role_exists,
                owner_matches,
                role_can_login,
                credential_digest: command.credential_digest.clone(),
                purge_audit_intent_digest: command.purge_audit_intent_digest.clone(),
                evidence_digest: String::new(),
            }
            .seal()
            .unwrap()
        }
    }

    impl PostgreSqlCommandExecutor for FakeExecutor {
        fn execute(
            &self,
            command: &PostgreSqlCommandV1,
            credential: Option<&SecretMaterial>,
        ) -> std::result::Result<PostgreSqlExecutionOutcomeV1, ProviderExecutionErrorV1> {
            command.validate().unwrap();
            if command.action == ResourceClaimActionKindV1::Ensure
                && credential.map(SecretMaterial::digest) != command.credential_digest
            {
                return Err(ProviderExecutionErrorV1 {
                    kind: ProviderFailureKindV1::Rejected,
                    code: "credential-digest-mismatch".to_string(),
                });
            }
            let mut completed = self.completed.lock().unwrap();
            if let Some((request_digest, evidence)) = completed.get(&command.idempotency_key) {
                if request_digest != &command.request_digest {
                    return Err(ProviderExecutionErrorV1 {
                        kind: ProviderFailureKindV1::IdempotencyConflict,
                        code: "same-key-different-request".to_string(),
                    });
                }
                return Ok(PostgreSqlExecutionOutcomeV1::Replayed {
                    evidence: evidence.clone(),
                });
            }
            let evidence = if command.action == ResourceClaimActionKindV1::Inspect {
                self.facts
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| Self::evidence_for(command))
            } else {
                Self::evidence_for(command)
            };
            completed.insert(
                command.idempotency_key.clone(),
                (command.request_digest.clone(), evidence.clone()),
            );
            *self
                .applied
                .lock()
                .unwrap()
                .entry(command.action)
                .or_default() += 1;
            if command.action != ResourceClaimActionKindV1::Inspect {
                *self.facts.lock().unwrap() = Some(evidence.clone());
            }
            let mut fail = self.apply_then_retry_once.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(ProviderExecutionErrorV1 {
                    kind: ProviderFailureKindV1::Retryable,
                    code: "lost-response-after-commit".to_string(),
                });
            }
            Ok(PostgreSqlExecutionOutcomeV1::Applied { evidence })
        }
    }

    fn ensure(
        claim: ResourceClaimV1,
        executor: &FakeExecutor,
        secrets: &FakeSecrets,
        random: &FakeRandom,
    ) -> ResourceClaimV1 {
        execute_resource_claim(
            claim,
            ResourceClaimActionV1::Ensure,
            &provider(),
            executor,
            secrets,
            random,
        )
        .unwrap()
    }

    fn purge_authorization(claim: &ResourceClaimV1) -> PurgeAuthorizationV1 {
        PurgeAuthorizationV1 {
            confirmation: claim.purge_confirmation(),
            audit_intent: PurgeAuditIntentV1 {
                intent_id: "audit-purge-001".to_string(),
                actor_id: "operator-001".to_string(),
                reason: "operator approved permanent test data deletion".to_string(),
                claim_digest: claim.claim_digest.clone(),
                generation: claim.generation,
            },
        }
    }

    fn purge_payload(claim: &ResourceClaimV1) -> orchestrator_runtime::ResourcePurgePayloadV1 {
        orchestrator_runtime::ResourcePurgePayloadV1 {
            schema_version: orchestrator_runtime::RESOURCE_PURGE_JOB_SCHEMA_VERSION.to_string(),
            node_id: "node-1".to_string(),
            claim_id: claim.identity.claim_id.clone(),
            claim_digest: claim.claim_digest.clone(),
            generation: claim.generation,
            confirmation: claim.purge_confirmation(),
            reason: "operator approved permanent test data deletion".to_string(),
            audit_intent: orchestrator_runtime::ResourcePurgeAuditIntentV1 {
                intent_id: "operation-resource-purge-001".to_string(),
                actor_id: "admin@example.test".to_string(),
                claim_digest: claim.claim_digest.clone(),
                generation: claim.generation,
            },
        }
    }

    #[test]
    fn claim_digest_names_and_plans_are_deterministic_and_injection_safe() {
        let claim = claim();
        let again = ResourceClaimV1::requested(identity(), 1, "postgresql-local").unwrap();
        assert_eq!(claim.claim_digest, again.claim_digest);
        let names = claim.postgres_names().unwrap();
        assert_eq!(names.database_name.len(), 59);
        assert_eq!(names.role_name.len(), 61);
        assert!(
            names
                .database_name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        );
        let credential = SecretMaterial::new(b"not-serialized".to_vec()).unwrap();
        let command = plan_postgresql_command(
            &claim,
            ResourceClaimActionKindV1::Ensure,
            Some(credential.digest()),
            None,
        )
        .unwrap();
        let json = serde_json::to_string(&command).unwrap();
        assert!(!json.contains("not-serialized"));
        assert!(json.contains(&claim.claim_digest));
        assert_eq!(
            command,
            plan_postgresql_command(
                &claim,
                ResourceClaimActionKindV1::Ensure,
                Some(credential.digest()),
                None,
            )
            .unwrap()
        );
    }

    #[test]
    fn live_postgres_configuration_rejects_mismatched_or_non_tls_admin_url() {
        let directory = tempfile::tempdir().unwrap();
        let base = PostgreSqlAdminConfigV1 {
            provider: provider(),
            admin_url: SecretMaterial::new(
                b"postgresql://admin:secret@other.internal:5432/postgres?sslmode=require".to_vec(),
            )
            .unwrap(),
            tls_trust: PostgreSqlTlsTrustV1::Platform,
            state_database: directory.path().join("receipts.sqlite3"),
        };
        assert!(LivePostgreSqlExecutor::new(base).is_err());

        let non_tls = PostgreSqlAdminConfigV1 {
            provider: PostgreSqlProviderDescriptorV1 {
                tls_mode: PostgreSqlTlsModeV1::Require,
                ..provider()
            },
            admin_url: SecretMaterial::new(
                b"postgresql://admin:secret@postgres.internal:5432/postgres?sslmode=disable"
                    .to_vec(),
            )
            .unwrap(),
            tls_trust: PostgreSqlTlsTrustV1::Platform,
            state_database: directory.path().join("receipts-2.sqlite3"),
        };
        assert!(LivePostgreSqlExecutor::new(non_tls).is_err());
    }

    #[test]
    fn serializable_claim_and_evidence_never_contain_password_or_dsn() {
        let executor = FakeExecutor::default();
        let secrets = FakeSecrets::default();
        let random = FakeRandom::default();
        let ready = ensure(claim(), &executor, &secrets, &random);
        assert_eq!(ready.status, ResourceClaimStatusV1::Ready);
        let serialized = serde_json::to_string(&ready).unwrap();
        let dsn = secrets
            .output(&ready.output_secret.as_ref().unwrap().reference)
            .unwrap();
        let dsn = std::str::from_utf8(dsn.expose_bytes()).unwrap();
        assert!(dsn.starts_with("postgresql://ojosrole_"));
        assert!(!serialized.contains("postgresql://"));
        assert!(!serialized.contains("01010101"));
        assert_eq!(
            ready.output_secret.as_ref().unwrap().mode,
            OUTPUT_SECRET_MODE
        );
    }

    #[test]
    fn filesystem_secret_store_is_durable_private_and_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileResourceSecretStore::new(directory.path()).unwrap();
        let first = store
            .get_or_create_0600(
                "claim-password",
                SecretMaterial::new(b"first-password".to_vec()).unwrap(),
            )
            .unwrap();
        let replay = FileResourceSecretStore::new(directory.path())
            .unwrap()
            .get_or_create_0600(
                "claim-password",
                SecretMaterial::new(b"different-candidate".to_vec()).unwrap(),
            )
            .unwrap();
        assert_eq!(first.expose_bytes(), replay.expose_bytes());

        let reference = "agent-secret://resource-outputs/abc/g1/dsn";
        store
            .write_output_0600(
                reference,
                SecretMaterial::new(b"postgresql://redacted".to_vec()).unwrap(),
            )
            .unwrap();
        store
            .write_output_0600(
                reference,
                SecretMaterial::new(b"postgresql://redacted".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(
            store
                .write_output_0600(
                    reference,
                    SecretMaterial::new(b"postgresql://replacement".to_vec()).unwrap(),
                )
                .is_err()
        );
        let path = store.output_path(reference).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"postgresql://redacted");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // SAFETY: geteuid/getegid take no pointers and have no preconditions.
            let expected_owner = unsafe { (libc::geteuid(), libc::getegid()) };
            for directory in [
                store.root().to_path_buf(),
                store.output_root().to_path_buf(),
            ] {
                let metadata = fs::metadata(directory).unwrap();
                assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
                assert_eq!((metadata.uid(), metadata.gid()), expected_owner);
            }
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let metadata = fs::metadata(&path).unwrap();
            assert_eq!((metadata.uid(), metadata.gid()), expected_owner);
            let lock = store.root().join(format!(
                "{}.lock",
                secret_component(&format!("output:{reference}"))
            ));
            let lock_metadata = fs::metadata(lock).unwrap();
            assert_eq!(lock_metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!((lock_metadata.uid(), lock_metadata.gid()), expected_owner);
        }
    }

    #[test]
    fn filesystem_secret_store_rejects_overlapping_or_symlinked_roots() {
        let directory = tempfile::tempdir().unwrap();
        let internal = directory.path().join("internal");
        fs::create_dir(&internal).unwrap();
        let nested = internal.join("outputs");
        assert!(
            FileResourceSecretStore::new_isolated_with_ownership(
                &internal,
                &nested,
                WorkloadFileOwnership::current_process(),
            )
            .is_err()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let real = directory.path().join("real-output");
            fs::create_dir(&real).unwrap();
            let linked = directory.path().join("linked-output");
            symlink(&real, &linked).unwrap();
            assert!(
                FileResourceSecretStore::new_isolated_with_ownership(
                    &internal,
                    &linked,
                    WorkloadFileOwnership::current_process(),
                )
                .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_resource_output_owner_must_match_agent_effective_identity() {
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: geteuid/getegid take no pointers and have no preconditions.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        FileResourceSecretStore::new_with_ownership(
            directory.path().join("matching"),
            WorkloadFileOwnership::Unix { uid, gid },
        )
        .unwrap();
        let wrong_uid = if uid == u32::MAX { uid - 1 } else { uid + 1 };
        let error = FileResourceSecretStore::new_with_ownership(
            directory.path().join("wrong"),
            WorkloadFileOwnership::Unix {
                uid: wrong_uid,
                gid,
            },
        )
        .unwrap_err();
        assert!(error.contains("refusing CAP_CHOWN fallback"));
    }

    #[cfg(windows)]
    #[test]
    fn explicit_unix_resource_output_owner_fails_closed_on_windows() {
        let directory = tempfile::tempdir().unwrap();
        let error = FileResourceSecretStore::new_with_ownership(
            directory.path().join("secrets"),
            WorkloadFileOwnership::standard_v3(),
        )
        .unwrap_err();
        assert!(error.contains("unsupported on this platform"));
    }

    #[test]
    fn output_reference_cannot_escape_private_root() {
        let directory = tempfile::tempdir().unwrap();
        let store = FileResourceSecretStore::new(directory.path()).unwrap();
        assert!(
            store
                .output_path("agent-secret://resource-outputs/../../outside")
                .is_err()
        );
        assert!(store.output_path("file:///outside").is_err());
    }

    #[test]
    fn lost_provider_response_retries_without_duplicate_mutation() {
        let executor = FakeExecutor::with_apply_then_retry();
        let secrets = FakeSecrets::default();
        let random = FakeRandom::default();
        let first = ensure(claim(), &executor, &secrets, &random);
        assert_eq!(first.status, ResourceClaimStatusV1::Provisioning);
        assert_eq!(executor.apply_count(ResourceClaimActionKindV1::Ensure), 1);
        let ready = ensure(first, &executor, &secrets, &random);
        assert_eq!(ready.status, ResourceClaimStatusV1::Ready);
        assert_eq!(executor.apply_count(ResourceClaimActionKindV1::Ensure), 1);
        assert_eq!(*random.calls.lock().unwrap(), 2);
        assert_eq!(secrets.internal.lock().unwrap().len(), 1);
    }

    #[test]
    fn crash_after_database_commit_before_output_secret_is_recoverable() {
        let executor = FakeExecutor::default();
        let secrets = FakeSecrets::with_output_crash();
        let random = FakeRandom::default();
        let first = ensure(claim(), &executor, &secrets, &random);
        assert_eq!(first.status, ResourceClaimStatusV1::Provisioning);
        assert_eq!(executor.apply_count(ResourceClaimActionKindV1::Ensure), 1);
        assert!(first.output_secret.is_none());

        let ready = ensure(first, &executor, &secrets, &random);
        assert_eq!(ready.status, ResourceClaimStatusV1::Ready);
        assert_eq!(executor.apply_count(ResourceClaimActionKindV1::Ensure), 1);
        assert!(
            secrets
                .output(&ready.output_secret.as_ref().unwrap().reference)
                .is_some()
        );
    }

    #[test]
    fn concurrent_ensure_uses_one_credential_and_one_provider_mutation() {
        let executor = Arc::new(FakeExecutor::default());
        let secrets = Arc::new(FakeSecrets::default());
        let random = Arc::new(FakeRandom::default());
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let executor = executor.clone();
            let secrets = secrets.clone();
            let random = random.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                ensure(claim(), &executor, &secrets, &random)
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(
            results
                .iter()
                .all(|claim| claim.status == ResourceClaimStatusV1::Ready)
        );
        assert_eq!(executor.apply_count(ResourceClaimActionKindV1::Ensure), 1);
        assert_eq!(secrets.internal.lock().unwrap().len(), 1);
        assert_eq!(secrets.outputs.lock().unwrap().len(), 1);
    }

    #[test]
    fn release_retains_database_disables_login_and_removes_only_output_binding() {
        let executor = FakeExecutor::default();
        let secrets = FakeSecrets::default();
        let random = FakeRandom::default();
        let ready = ensure(claim(), &executor, &secrets, &random);
        let reference = ready.output_secret.as_ref().unwrap().reference.clone();
        let retained = execute_resource_claim(
            ready,
            ResourceClaimActionV1::Release,
            &provider(),
            &executor,
            &secrets,
            &random,
        )
        .unwrap();
        assert_eq!(retained.status, ResourceClaimStatusV1::Retained);
        assert!(retained.output_secret.is_none());
        assert!(retained.evidence.as_ref().unwrap().database_exists);
        assert!(!retained.evidence.as_ref().unwrap().role_can_login);
        assert!(secrets.output(&reference).is_none());
        assert_eq!(secrets.internal.lock().unwrap().len(), 1);
    }

    #[test]
    fn purge_requires_exact_confirmation_and_bound_audit_intent() {
        let executor = FakeExecutor::default();
        let secrets = FakeSecrets::default();
        let random = FakeRandom::default();
        let ready = ensure(claim(), &executor, &secrets, &random);
        let retained = execute_resource_claim(
            ready,
            ResourceClaimActionV1::Release,
            &provider(),
            &executor,
            &secrets,
            &random,
        )
        .unwrap();
        let mut wrong = purge_authorization(&retained);
        wrong.confirmation.push('!');
        assert!(matches!(
            execute_resource_claim(
                retained.clone(),
                ResourceClaimActionV1::Purge {
                    authorization: wrong
                },
                &provider(),
                &executor,
                &secrets,
                &random
            ),
            Err(ResourceClaimError::PurgeConfirmationMismatch { .. })
        ));
        assert!(retained.evidence.as_ref().unwrap().database_exists);

        let authorization = purge_authorization(&retained);
        let expected_audit = authorization.audit_intent.digest().unwrap();
        let deleted = execute_resource_claim(
            retained,
            ResourceClaimActionV1::Purge { authorization },
            &provider(),
            &executor,
            &secrets,
            &random,
        )
        .unwrap();
        assert_eq!(deleted.status, ResourceClaimStatusV1::Deleted);
        assert_eq!(
            deleted.purge_audit_intent_digest.as_deref(),
            Some(expected_audit.as_str())
        );
        assert_eq!(
            deleted
                .evidence
                .as_ref()
                .unwrap()
                .purge_audit_intent_digest
                .as_deref(),
            Some(expected_audit.as_str())
        );
        assert!(secrets.internal.lock().unwrap().is_empty());
    }

    #[test]
    fn purge_is_impossible_before_release_even_with_valid_confirmation() {
        let executor = FakeExecutor::default();
        let secrets = FakeSecrets::default();
        let random = FakeRandom::default();
        let ready = ensure(claim(), &executor, &secrets, &random);
        let authorization = purge_authorization(&ready);
        assert!(matches!(
            execute_resource_claim(
                ready,
                ResourceClaimActionV1::Purge { authorization },
                &provider(),
                &executor,
                &secrets,
                &random
            ),
            Err(ResourceClaimError::ActionNotAllowed {
                action: ResourceClaimActionKindV1::Purge,
                status: ResourceClaimStatusV1::Ready
            })
        ));
    }

    #[test]
    fn idempotency_key_conflict_becomes_needs_attention() {
        let executor = FakeExecutor::default();
        let secrets = FakeSecrets::default();
        let random = FakeRandom::default();
        let first = ensure(claim(), &executor, &secrets, &random);
        assert_eq!(first.status, ResourceClaimStatusV1::Ready);

        secrets.internal.lock().unwrap().clear();
        let conflicted = ensure(first, &executor, &secrets, &random);
        assert_eq!(conflicted.status, ResourceClaimStatusV1::NeedsAttention);
        assert_eq!(
            conflicted.failure.as_ref().unwrap().code,
            ResourceClaimFailureCodeV1::IdempotencyConflict
        );
    }

    #[test]
    fn illegal_status_transitions_and_tampered_digest_fail_closed() {
        let mut claim = claim();
        assert!(matches!(
            transition(&mut claim, ResourceClaimStatusV1::Deleted),
            Err(ResourceClaimError::InvalidStatusTransition { .. })
        ));
        claim.claim_digest = format!("sha256:{}", "f".repeat(64));
        assert!(matches!(
            claim.validate(),
            Err(ResourceClaimError::ClaimDigestMismatch { .. })
        ));
    }

    #[test]
    fn manager_restart_reuses_ready_claim_output_without_provider_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let secret_root = directory.path().join("secrets");
        let state_database = directory.path().join("claims.sqlite3");
        let step = runtime_step("deployment-v1");

        let first_manager = LocalResourceClaimManager::new(
            provider(),
            FakeExecutor::default(),
            FileResourceSecretStore::new(&secret_root).unwrap(),
            &state_database,
        )
        .unwrap();
        let first = first_manager.ensure(&step).unwrap();
        let first_output = first.output_secret.clone().unwrap();
        let first_bytes = fs::read(
            first_manager
                .secrets
                .output_path(&first_output.reference)
                .unwrap(),
        )
        .unwrap();
        drop(first_manager);

        let restarted = LocalResourceClaimManager::new(
            provider(),
            FakeExecutor::default(),
            FileResourceSecretStore::new(&secret_root).unwrap(),
            &state_database,
        )
        .unwrap();
        let replay = restarted.ensure(&step).unwrap();
        assert_eq!(replay.claim_digest, first.claim_digest);
        assert_eq!(replay.output_secret.as_ref(), Some(&first_output));
        assert_eq!(
            fs::read(restarted.output_path(&first_output.reference).unwrap()).unwrap(),
            first_bytes
        );
        assert_eq!(
            restarted
                .executor
                .apply_count(ResourceClaimActionKindV1::Ensure),
            0
        );
    }

    #[test]
    fn stable_owner_replacement_reuses_generation_one_and_last_uninstall_retains() {
        let directory = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::default();
        let store = FileResourceSecretStore::new(directory.path().join("secrets")).unwrap();
        let manager = LocalResourceClaimManager::new(
            provider(),
            executor,
            store,
            directory.path().join("claims.sqlite3"),
        )
        .unwrap();

        let old = runtime_step("deployment-v1");
        let ready = manager.ensure(&old).unwrap();
        assert_eq!(ready.generation, 1);
        assert_eq!(ready.identity.owner_instance_id, old.owner_instance_id);

        let replacement = runtime_step("deployment-v2");
        let reused = manager
            .reuse_for_replacement("deployment-v1", std::slice::from_ref(&replacement))
            .unwrap();
        assert_eq!(reused[0].claim_digest, ready.claim_digest);
        assert_eq!(
            reused[0].postgres_names().unwrap(),
            ready.postgres_names().unwrap()
        );
        manager
            .bind_replacement(
                "deployment-v1",
                "deployment-v2",
                std::slice::from_ref(&replacement.claim_id),
            )
            .unwrap();

        let old_release = manager.release_deployment("deployment-v1").unwrap();
        assert_eq!(old_release.len(), 1);
        assert!(!old_release[0].provider_released);
        assert_eq!(old_release[0].claim.status, ResourceClaimStatusV1::Ready);

        let final_release = manager.release_deployment("deployment-v2").unwrap();
        assert_eq!(final_release.len(), 1);
        assert!(final_release[0].provider_released);
        assert_eq!(
            final_release[0].claim.status,
            ResourceClaimStatusV1::Retained
        );
        assert_eq!(
            manager
                .executor
                .apply_count(ResourceClaimActionKindV1::Ensure),
            1
        );
        assert_eq!(
            manager
                .executor
                .apply_count(ResourceClaimActionKindV1::Release),
            1
        );
    }

    #[test]
    fn manager_purge_requires_retained_zero_binding_then_persists_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let manager = LocalResourceClaimManager::new(
            provider(),
            FakeExecutor::default(),
            FileResourceSecretStore::new(directory.path().join("secrets")).unwrap(),
            directory.path().join("claims.sqlite3"),
        )
        .unwrap();
        let step = runtime_step("deployment-v1");
        let ready = manager.ensure(&step).unwrap();

        // Simulate an impossible-but-durable stale binding to prove the
        // destructive boundary checks its local binding table, not just status.
        let retained = execute_resource_claim(
            ready,
            ResourceClaimActionV1::Release,
            &manager.provider,
            &manager.executor,
            &manager.secrets,
            &manager.random,
        )
        .unwrap();
        manager.save(&retained).unwrap();
        let error = manager.purge(&purge_payload(&retained)).unwrap_err();
        assert!(error.to_string().contains("still has a deployment binding"));
        assert_eq!(
            manager.load(&step.claim_id).unwrap().unwrap().status,
            ResourceClaimStatusV1::Retained
        );

        manager
            .state
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM resource_claim_bindings WHERE claim_id=?1",
                params![step.claim_id],
            )
            .unwrap();
        let deleted = manager.purge(&purge_payload(&retained)).unwrap();
        assert_eq!(deleted.status, ResourceClaimStatusV1::Deleted);
        assert_eq!(
            manager.load(&step.claim_id).unwrap().unwrap().status,
            ResourceClaimStatusV1::Deleted
        );
        assert_eq!(
            manager
                .executor
                .apply_count(ResourceClaimActionKindV1::Purge),
            1
        );
    }

    struct UnknownPurgeExecutor {
        base: FakeExecutor,
    }

    impl PostgreSqlCommandExecutor for UnknownPurgeExecutor {
        fn execute(
            &self,
            command: &PostgreSqlCommandV1,
            credential: Option<&SecretMaterial>,
        ) -> std::result::Result<PostgreSqlExecutionOutcomeV1, ProviderExecutionErrorV1> {
            if command.action == ResourceClaimActionKindV1::Purge {
                return Err(ProviderExecutionErrorV1 {
                    kind: ProviderFailureKindV1::FactUnknown,
                    code: "simulated-purge-outcome-unknown".to_string(),
                });
            }
            self.base.execute(command, credential)
        }
    }

    #[test]
    fn manager_purge_unknown_provider_fact_persists_needs_attention() {
        let directory = tempfile::tempdir().unwrap();
        let manager = LocalResourceClaimManager::new(
            provider(),
            UnknownPurgeExecutor {
                base: FakeExecutor::default(),
            },
            FileResourceSecretStore::new(directory.path().join("secrets")).unwrap(),
            directory.path().join("claims.sqlite3"),
        )
        .unwrap();
        let step = runtime_step("deployment-v1");
        manager.ensure(&step).unwrap();
        let retained = manager
            .release_deployment("deployment-v1")
            .unwrap()
            .remove(0)
            .claim;
        let attention = manager.purge(&purge_payload(&retained)).unwrap();
        assert_eq!(attention.status, ResourceClaimStatusV1::NeedsAttention);
        assert_eq!(
            attention.failure.as_ref().unwrap().code,
            ResourceClaimFailureCodeV1::ProviderFactUnknown
        );
        assert_eq!(
            manager.load(&step.claim_id).unwrap().unwrap().status,
            ResourceClaimStatusV1::NeedsAttention
        );
    }

    #[test]
    fn replacement_exact_set_and_generation_drift_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let manager = LocalResourceClaimManager::new(
            provider(),
            FakeExecutor::default(),
            FileResourceSecretStore::new(directory.path().join("secrets")).unwrap(),
            directory.path().join("claims.sqlite3"),
        )
        .unwrap();
        manager.ensure(&runtime_step("deployment-v1")).unwrap();

        assert!(manager.reuse_for_replacement("deployment-v1", &[]).is_err());
        let mut drifted = runtime_step("deployment-v2");
        drifted.generation = 2;
        assert!(
            manager
                .reuse_for_replacement("deployment-v1", &[drifted])
                .is_err()
        );
        let mut second_claim = runtime_step("deployment-v2");
        second_claim.claim_id = "claim-contest-second-database".to_string();
        assert!(
            manager
                .reuse_for_replacement(
                    "deployment-v1",
                    &[runtime_step("deployment-v2"), second_claim]
                )
                .is_err()
        );
    }
}
