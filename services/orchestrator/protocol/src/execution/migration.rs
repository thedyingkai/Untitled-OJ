//! Deterministic migration contracts; no environment reads or I/O.

use super::OciImageReference;
use super::validation::{validate_safe_resource_name, validate_sha256_text};
use crate::RuntimeError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MIGRATION_RUNTIME_ROLE_LABEL: &str = "ojos.runtime_role";

pub const MIGRATION_RUNTIME_ROLE: &str = "migration";

pub const MIGRATION_MANAGED_BY_LABEL: &str = "ojos.migration_managed_by";

pub const MIGRATION_MANAGED_BY: &str = "orchestrator-agent";

pub const MIGRATION_JOB_ID_LABEL: &str = "ojos.migration_job_id";

pub const MIGRATION_SERVICE_LABEL: &str = "ojos.migration_service";

pub const MIGRATION_VERSION_LABEL: &str = "ojos.migration_version";

pub const MIGRATION_CHECKSUM_LABEL: &str = "ojos.migration_checksum";

pub const MIGRATION_IDENTITY_LABEL: &str = "ojos.migration_identity_sha256";

pub const MIGRATION_RESOURCE_CLAIMS_LABEL: &str = "ojos.migration_resource_claims_sha256";

/// Hashes only stable, non-sensitive ResourceClaim logical names. Host paths,
/// DSNs and secret material cannot enter a migration label.
pub fn migration_resource_claims_sha256(
    resource_claims: &[String],
) -> Result<String, RuntimeError> {
    let mut names = resource_claims.to_vec();
    names.sort();
    if names.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(RuntimeError::InvalidRuntimeContext(
            "migration ResourceClaim names must be unique".to_string(),
        ));
    }
    for name in &names {
        validate_safe_resource_name(name)?;
    }
    let canonical = serde_json::to_vec(&names).map_err(|error| {
        RuntimeError::InvalidRuntimeContext(format!(
            "cannot encode migration ResourceClaim identity: {error}"
        ))
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

pub fn migration_identity_sha256(
    service_name: &str,
    version: &str,
    checksum: &str,
    image: &OciImageReference,
    resource_claims_sha256: &str,
) -> Result<String, RuntimeError> {
    validate_migration_label_token("migration service_name", service_name, 63)?;
    validate_migration_label_token("migration version", version, 128)?;
    validate_sha256_text("migration checksum", checksum)?;
    validate_sha256_text("migration resource_claims_sha256", resource_claims_sha256)?;
    let image = image.to_string();
    let mut digest = Sha256::new();
    digest.update(b"ojos.migration.identity.v1\0");
    for value in [
        service_name,
        version,
        checksum,
        image.as_str(),
        resource_claims_sha256,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

pub fn validate_migration_label_token(
    field: &str,
    value: &str,
    max_len: usize,
) -> Result<(), RuntimeError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > max_len
        || (!bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit())
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':' | b'+')
        })
    {
        return Err(RuntimeError::InvalidRuntimeContext(format!(
            "{field} must be a bounded lowercase label token"
        )));
    }
    Ok(())
}

/// Exact, credential-free identity carried by every one-shot migration
/// container. The identity digest binds the immutable migration artifact and
/// the names (never values or paths) of its ResourceClaims.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MigrationContainerIdentityV1 {
    pub job_id: String,
    pub service_name: String,
    pub version: String,
    pub checksum: String,
    pub image: String,
    pub resource_claims_sha256: String,
    pub identity_sha256: String,
}

impl MigrationContainerIdentityV1 {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        validate_migration_label_token("migration job_id", &self.job_id, 128)?;
        validate_migration_label_token("migration service_name", &self.service_name, 63)?;
        validate_migration_label_token("migration version", &self.version, 128)?;
        validate_sha256_text("migration checksum", &self.checksum)?;
        validate_sha256_text(
            "migration resource_claims_sha256",
            &self.resource_claims_sha256,
        )?;
        let image = OciImageReference::parse(&self.image).map_err(|_| {
            RuntimeError::InvalidRuntimeContext(
                "migration image label is not a canonical OCI digest reference".to_string(),
            )
        })?;
        let expected = migration_identity_sha256(
            &self.service_name,
            &self.version,
            &self.checksum,
            &image,
            &self.resource_claims_sha256,
        )?;
        if self.identity_sha256 != expected {
            return Err(RuntimeError::InvalidRuntimeContext(
                "migration identity digest does not match its immutable labels".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MigrationContainerStateV1 {
    Created,
    Running,
    Paused,
    Restarting,
    Stopped,
    Exited,
    Unknown,
}

impl MigrationContainerStateV1 {
    pub fn is_proven_inactive(self) -> bool {
        matches!(self, Self::Created | Self::Stopped | Self::Exited)
    }
}

/// A deliberately closed projection of migration labels. Arbitrary Docker
/// labels are never returned to the Agent or control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MigrationContainerObservationV1 {
    pub container_id: String,
    pub observed_state: MigrationContainerStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<MigrationContainerIdentityV1>,
    #[serde(default)]
    pub validation_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MigrationContainerInventoryV1 {
    pub inventory_complete: bool,
    #[serde(default)]
    pub inventory_error: String,
    pub containers: Vec<MigrationContainerObservationV1>,
}
