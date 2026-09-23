use crate::{ServiceContractV3, contract_bytes};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const RESOLVED_ARTIFACTS_SCHEMA_VERSION: &str = "ojos.dev/resolved-artifacts/v1";
pub const RELEASE_LOCK_SCHEMA_VERSION: &str = "ojos.dev/release-lock/v1";
pub const CONTRACT_SLOT: &str = "contract";
pub const EVENTS_MANIFEST_SLOT: &str = "events";
pub const SBOM_SLOT: &str = "sbom";
pub const PROVENANCE_SLOT: &str = "provenance";
pub const CONFIG_SCHEMA_SLOT: &str = "config.schema";

#[derive(Debug, Error)]
pub enum SealError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("resolved artifact document is invalid: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("canonical contract serialization failed: {0}")]
    Contract(#[from] crate::CompilerError),
    #[error(
        "resolved artifact schemaVersion must be {RESOLVED_ARTIFACTS_SCHEMA_VERSION}, found {0}"
    )]
    SchemaVersion(String),
    #[error("artifact slot is invalid: {0}")]
    InvalidSlot(String),
    #[error("artifact {slot} has invalid mediaType {media_type:?}")]
    InvalidMediaType { slot: String, media_type: String },
    #[error("artifact {slot} digest must be canonical sha256:<64 lowercase hex>, found {digest}")]
    InvalidDigest { slot: String, digest: String },
    #[error("artifact {0} size must be greater than zero")]
    InvalidSize(String),
    #[error("required artifact slot {slot} is missing for role {role}")]
    MissingSlot { role: String, slot: String },
    #[error("artifact slot {slot} is assigned conflicting roles {first_role} and {second_role}")]
    ConflictingSlot {
        slot: String,
        first_role: String,
        second_role: String,
    },
    #[error("artifact {slot} for role {role} has digest {actual}, expected {expected}")]
    DigestMismatch {
        role: String,
        slot: String,
        expected: String,
        actual: String,
    },
    #[error("artifact {slot} for role {role} has size {actual}, expected {expected}")]
    SizeMismatch {
        role: String,
        slot: String,
        expected: u64,
        actual: u64,
    },
    #[error("release lock verification failed")]
    LockMismatch,
}

pub type Result<T> = std::result::Result<T, SealError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedArtifactsV1 {
    pub schema_version: String,
    pub artifacts: BTreeMap<String, ResolvedArtifactV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedArtifactV1 {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    /// Immutable OCI reference or HTTPS content-addressed artifact URL. The
    /// build seal may be created before publication; the publish gate requires
    /// references for executable and externally served subjects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseLockV1 {
    pub schema_version: String,
    pub service_id: String,
    pub service_version: Version,
    pub source_digest: String,
    pub contract_digest: String,
    pub artifacts: BTreeMap<String, ResolvedArtifactV1>,
    pub bindings: Vec<ArtifactBindingV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactBindingV1 {
    pub role: String,
    pub slot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactRequirement {
    pub role: String,
    pub slot: String,
    pub expected_digest: Option<String>,
    pub expected_size: Option<u64>,
}

pub fn openapi_slot(api_id: &str) -> String {
    format!("openapi.{api_id}")
}

pub fn event_schema_slot(event_type: &str, version: u32) -> String {
    format!("event.{event_type}.v{version}.schema")
}

pub fn frontend_manifest_slot(module_id: &str) -> String {
    format!("frontend.{module_id}.manifest")
}

/// Returns every slot that must be part of the signed Catalog subject graph.
///
/// Source-owned documents carry an expected digest. OCI/runtime/bundle slots
/// are resolved by the build system and become immutable when this lock is
/// signed.
pub fn artifact_requirements(contract: &ServiceContractV3) -> Result<Vec<ArtifactRequirement>> {
    let canonical_contract = contract_bytes(contract)?;
    let mut requirements = BTreeMap::<String, ArtifactRequirement>::new();

    insert_requirement(
        &mut requirements,
        ArtifactRequirement {
            role: "contract".to_string(),
            slot: CONTRACT_SLOT.to_string(),
            expected_digest: Some(digest(&canonical_contract)),
            expected_size: Some(canonical_contract.len() as u64),
        },
    )?;
    insert_requirement(
        &mut requirements,
        ArtifactRequirement {
            role: "runtime".to_string(),
            slot: contract.runtime.artifact.clone(),
            expected_digest: None,
            expected_size: None,
        },
    )?;

    let mut api_surfaces = contract.api_surfaces.iter().collect::<Vec<_>>();
    api_surfaces.sort_by(|left, right| left.api_id.cmp(&right.api_id));
    for api in api_surfaces {
        insert_requirement(
            &mut requirements,
            ArtifactRequirement {
                role: format!("openapi:{}", api.api_id),
                slot: openapi_slot(&api.api_id),
                expected_digest: Some(api.document_digest.clone()),
                expected_size: None,
            },
        )?;
    }

    let mut events = contract
        .events
        .publishes
        .iter()
        .chain(contract.events.subscribes.iter())
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        left.event_type
            .cmp(&right.event_type)
            .then(left.version.cmp(&right.version))
            .then(left.schema.digest.cmp(&right.schema.digest))
    });
    for event in events {
        insert_requirement(
            &mut requirements,
            ArtifactRequirement {
                role: format!("event-schema:{}:v{}", event.event_type, event.version),
                slot: event_schema_slot(&event.event_type, event.version),
                expected_digest: Some(event.schema.digest.clone()),
                expected_size: None,
            },
        )?;
    }
    let canonical_events = serde_json_canonicalizer::to_vec(&contract.events)?;
    insert_requirement(
        &mut requirements,
        ArtifactRequirement {
            role: "events-manifest".to_string(),
            slot: EVENTS_MANIFEST_SLOT.to_string(),
            expected_digest: Some(digest(&canonical_events)),
            expected_size: Some(canonical_events.len() as u64),
        },
    )?;

    let mut migrations = contract.migrations.iter().collect::<Vec<_>>();
    migrations.sort_by(|left, right| left.id.cmp(&right.id));
    for migration in migrations {
        insert_requirement(
            &mut requirements,
            ArtifactRequirement {
                role: format!("migration:{}", migration.id),
                slot: migration.artifact.clone(),
                expected_digest: None,
                expected_size: None,
            },
        )?;
    }

    let mut frontends = contract.frontends.iter().collect::<Vec<_>>();
    frontends.sort_by(|left, right| left.module.module_id.cmp(&right.module.module_id));
    for frontend in frontends {
        insert_requirement(
            &mut requirements,
            ArtifactRequirement {
                role: format!("frontend-manifest:{}", frontend.module.module_id),
                slot: frontend_manifest_slot(&frontend.module.module_id),
                expected_digest: Some(frontend.manifest.digest.clone()),
                expected_size: None,
            },
        )?;
        insert_requirement(
            &mut requirements,
            ArtifactRequirement {
                role: format!("frontend-bundle:{}", frontend.module.module_id),
                slot: frontend.module.artifact.clone(),
                expected_digest: None,
                expected_size: None,
            },
        )?;
    }

    if let Some(config) = &contract.config_schema {
        insert_requirement(
            &mut requirements,
            ArtifactRequirement {
                role: "config-schema".to_string(),
                slot: CONFIG_SCHEMA_SLOT.to_string(),
                expected_digest: Some(config.digest.clone()),
                expected_size: None,
            },
        )?;
    }

    insert_requirement(
        &mut requirements,
        ArtifactRequirement {
            role: "sbom".to_string(),
            slot: SBOM_SLOT.to_string(),
            expected_digest: None,
            expected_size: None,
        },
    )?;
    insert_requirement(
        &mut requirements,
        ArtifactRequirement {
            role: "provenance".to_string(),
            slot: PROVENANCE_SLOT.to_string(),
            expected_digest: None,
            expected_size: None,
        },
    )?;

    Ok(requirements.into_values().collect())
}

pub fn parse_resolved_artifacts(bytes: &[u8]) -> Result<ResolvedArtifactsV1> {
    let resolved: ResolvedArtifactsV1 = serde_json::from_slice(bytes)?;
    validate_resolved_artifacts(&resolved)?;
    Ok(resolved)
}

pub fn load_resolved_artifacts(path: &Path) -> Result<ResolvedArtifactsV1> {
    let bytes = fs::read(path).map_err(|source| SealError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_resolved_artifacts(&bytes)
}

pub fn validate_resolved_artifacts(resolved: &ResolvedArtifactsV1) -> Result<()> {
    if resolved.schema_version != RESOLVED_ARTIFACTS_SCHEMA_VERSION {
        return Err(SealError::SchemaVersion(resolved.schema_version.clone()));
    }
    for (slot, artifact) in &resolved.artifacts {
        validate_slot(slot)?;
        validate_artifact(slot, artifact)?;
    }
    Ok(())
}

pub fn seal(contract: &ServiceContractV3, resolved: &ResolvedArtifactsV1) -> Result<ReleaseLockV1> {
    validate_resolved_artifacts(resolved)?;
    let requirements = artifact_requirements(contract)?;
    for requirement in &requirements {
        let artifact =
            resolved
                .artifacts
                .get(&requirement.slot)
                .ok_or_else(|| SealError::MissingSlot {
                    role: requirement.role.clone(),
                    slot: requirement.slot.clone(),
                })?;
        if let Some(expected) = &requirement.expected_digest
            && &artifact.digest != expected
        {
            return Err(SealError::DigestMismatch {
                role: requirement.role.clone(),
                slot: requirement.slot.clone(),
                expected: expected.clone(),
                actual: artifact.digest.clone(),
            });
        }
        if let Some(expected) = requirement.expected_size
            && artifact.size != expected
        {
            return Err(SealError::SizeMismatch {
                role: requirement.role.clone(),
                slot: requirement.slot.clone(),
                expected,
                actual: artifact.size,
            });
        }
    }
    let canonical_contract = contract_bytes(contract)?;
    Ok(ReleaseLockV1 {
        schema_version: RELEASE_LOCK_SCHEMA_VERSION.to_string(),
        service_id: contract.service_id.clone(),
        service_version: contract.service_version.clone(),
        source_digest: contract.source_digest.clone(),
        contract_digest: digest(&canonical_contract),
        artifacts: resolved.artifacts.clone(),
        bindings: requirements
            .into_iter()
            .map(|requirement| ArtifactBindingV1 {
                role: requirement.role,
                slot: requirement.slot,
            })
            .collect(),
    })
}

pub fn release_lock_bytes(lock: &ReleaseLockV1) -> Result<Vec<u8>> {
    Ok(serde_json_canonicalizer::to_vec(lock)?)
}

pub fn release_lock_digest(lock: &ReleaseLockV1) -> Result<String> {
    Ok(digest(&release_lock_bytes(lock)?))
}

pub fn write_release_lock(lock: &ReleaseLockV1, path: &Path) -> Result<()> {
    let bytes = release_lock_bytes(lock)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SealError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, bytes).map_err(|source| SealError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Rebuilds the expected lock from the contract and its resolved artifact map.
/// This catches role/slot substitution even before signature verification.
pub fn verify_release_lock(contract: &ServiceContractV3, lock: &ReleaseLockV1) -> Result<()> {
    if lock.schema_version != RELEASE_LOCK_SCHEMA_VERSION {
        return Err(SealError::LockMismatch);
    }
    let rebuilt = seal(
        contract,
        &ResolvedArtifactsV1 {
            schema_version: RESOLVED_ARTIFACTS_SCHEMA_VERSION.to_string(),
            artifacts: lock.artifacts.clone(),
        },
    )?;
    if &rebuilt != lock {
        return Err(SealError::LockMismatch);
    }
    Ok(())
}

fn insert_requirement(
    requirements: &mut BTreeMap<String, ArtifactRequirement>,
    requirement: ArtifactRequirement,
) -> Result<()> {
    validate_slot(&requirement.slot)?;
    match requirements.entry(requirement.slot.clone()) {
        Entry::Vacant(entry) => {
            entry.insert(requirement);
        }
        Entry::Occupied(entry) => {
            let existing = entry.get();
            if existing.role == requirement.role
                && existing.expected_digest == requirement.expected_digest
                && existing.expected_size == requirement.expected_size
            {
                return Ok(());
            }
            return Err(SealError::ConflictingSlot {
                slot: requirement.slot,
                first_role: existing.role.clone(),
                second_role: requirement.role,
            });
        }
    }
    Ok(())
}

fn validate_slot(slot: &str) -> Result<()> {
    let pattern = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$").expect("valid slot regex");
    if !pattern.is_match(slot) {
        return Err(SealError::InvalidSlot(slot.to_string()));
    }
    Ok(())
}

fn validate_artifact(slot: &str, artifact: &ResolvedArtifactV1) -> Result<()> {
    if artifact.media_type.trim() != artifact.media_type
        || artifact
            .media_type
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || !artifact.media_type.contains('/')
    {
        return Err(SealError::InvalidMediaType {
            slot: slot.to_string(),
            media_type: artifact.media_type.clone(),
        });
    }
    if !is_canonical_sha256(&artifact.digest) {
        return Err(SealError::InvalidDigest {
            slot: slot.to_string(),
            digest: artifact.digest.clone(),
        });
    }
    if artifact.size == 0 {
        return Err(SealError::InvalidSize(slot.to_string()));
    }
    if let Some(reference) = &artifact.reference
        && (reference.trim() != reference
            || reference.is_empty()
            || reference.chars().any(char::is_whitespace))
    {
        return Err(SealError::InvalidSlot(format!(
            "{slot} has invalid immutable reference"
        )));
    }
    Ok(())
}

fn is_canonical_sha256(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
