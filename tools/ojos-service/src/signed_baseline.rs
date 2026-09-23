use anyhow::{Context, Result, ensure};
use ojos_service::{
    ServiceContractV3, contract_bytes,
    seal::{
        CONTRACT_SLOT, ReleaseLockV1, release_lock_bytes, release_lock_digest, verify_release_lock,
    },
};
use orchestrator_core::ServiceReleaseContract;
use orchestrator_manager::catalog_v2::{
    CatalogTrustStore, CatalogV2, ReleaseChannel, TargetPlatform,
};
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, de};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedBaselineReport {
    pub trust: &'static str,
    pub catalog: PathBuf,
    pub external_trust_store: PathBuf,
    pub catalog_id: String,
    pub verified_key_ids: Vec<String>,
    pub service_id: String,
    pub service_version: Version,
    pub target: String,
    pub metadata: PathBuf,
    pub release_lock: PathBuf,
    pub service_contract: PathBuf,
}

pub struct VerifiedSignedBaseline {
    pub contract: ServiceContractV3,
    pub report: SignedBaselineReport,
}

/// Loads a previous compiler contract only after proving its complete offline
/// Catalog v2 chain against an operator-owned trust store. No URL in Catalog
/// metadata is opened: the signed publication's fixed sibling layout is the
/// only accepted source of metadata, lock, and canonical contract bytes.
pub fn load_signed_baseline(
    catalog_directory: &Path,
    external_trust_file: &Path,
    current: &ServiceContractV3,
    target: &TargetPlatform,
) -> Result<VerifiedSignedBaseline> {
    ensure_external_trust(catalog_directory, external_trust_file)?;
    let trust = load_external_trust(external_trust_file)?;

    let catalog_path = catalog_directory.join("catalog.json");
    let catalog_bytes = read(&catalog_path)?;
    let catalog: CatalogV2 = serde_json::from_slice(&catalog_bytes)
        .with_context(|| format!("parse signed Catalog {}", catalog_path.display()))?;
    let verified = catalog
        .validate_trusted(&trust)
        .context("verify every Catalog v2 signature against external trust store")?;

    let module = catalog.module(&current.service_id).with_context(|| {
        format!(
            "signed Catalog {} has no module {}",
            catalog.id, current.service_id
        )
    })?;
    let release = module
        .releases
        .iter()
        .filter(|release| {
            release.channel == ReleaseChannel::Stable
                && release.version < current.service_version
                && release
                    .platforms
                    .iter()
                    .any(|supported| supported.supports(target))
        })
        .max_by(|left, right| left.version.cmp(&right.version))
        .with_context(|| {
            format!(
                "signed Catalog has no prior stable {} release below {} for {}",
                current.service_id, current.service_version, target
            )
        })?;

    let stem = format!("{}-{}", current.service_id, release.version);
    let metadata_path = catalog_directory
        .join("metadata")
        .join(format!("{stem}.release.json"));
    let metadata_bytes = read(&metadata_path)?;
    ensure!(
        digest(&metadata_bytes) == release.metadata.sha256.as_str(),
        "signed Catalog metadata digest mismatch for {} {}",
        current.service_id,
        release.version
    );
    let metadata_value = serde_json::from_slice(&metadata_bytes)
        .with_context(|| format!("parse release metadata {}", metadata_path.display()))?;
    let metadata = ServiceReleaseContract::from_json_value(metadata_value).with_context(|| {
        format!(
            "validate strict release metadata {}",
            metadata_path.display()
        )
    })?;
    ensure!(
        metadata.release.service_name == current.service_id,
        "release metadata service {} does not match Catalog module {}",
        metadata.release.service_name,
        current.service_id
    );
    ensure!(
        metadata.release.version == release.version.to_string(),
        "release metadata version {} does not match Catalog release {}",
        metadata.release.version,
        release.version
    );
    let platform = metadata
        .platform
        .as_ref()
        .context("signed release metadata omits platform contract")?;

    let lock_path = catalog_directory
        .join("metadata")
        .join(format!("{stem}.release.lock.json"));
    let lock_bytes = read(&lock_path)?;
    ensure!(
        digest(&lock_bytes) == platform.release_lock_digest,
        "release lock byte digest differs from signed metadata platform.releaseLockDigest"
    );
    let lock: ReleaseLockV1 = serde_json::from_slice(&lock_bytes)
        .with_context(|| format!("parse release lock {}", lock_path.display()))?;
    ensure!(
        release_lock_bytes(&lock)? == lock_bytes,
        "release lock must use canonical JCS bytes"
    );
    ensure!(
        release_lock_digest(&lock)? == platform.release_lock_digest,
        "canonical release lock digest differs from signed metadata"
    );
    ensure!(
        lock.service_id == current.service_id && lock.service_version == release.version,
        "release lock service/version does not match selected Catalog release"
    );

    let contract_path = catalog_directory
        .join("metadata")
        .join(format!("{stem}.service.contract.json"));
    let canonical_contract = read(&contract_path)?;
    let contract_slot = lock
        .artifacts
        .get(CONTRACT_SLOT)
        .context("release lock omits contract artifact slot")?;
    ensure!(
        digest(&canonical_contract) == contract_slot.digest,
        "canonical service contract digest differs from release lock contract slot"
    );
    ensure!(
        canonical_contract.len() as u64 == contract_slot.size,
        "canonical service contract size differs from release lock contract slot"
    );
    ensure!(
        lock.contract_digest == contract_slot.digest
            && platform.contract_digest == contract_slot.digest,
        "release lock and signed platform contract digests do not bind the same contract"
    );
    let platform_subject = platform
        .artifact_subjects
        .iter()
        .find(|subject| subject.slot == CONTRACT_SLOT)
        .context("signed platform artifact graph omits contract subject")?;
    ensure!(
        platform_subject.digest == contract_slot.digest
            && platform_subject.size == contract_slot.size,
        "signed platform contract subject differs from release lock contract slot"
    );

    let previous: ServiceContractV3 = serde_json::from_slice(&canonical_contract)
        .with_context(|| format!("parse canonical contract {}", contract_path.display()))?;
    ensure!(
        contract_bytes(&previous)? == canonical_contract,
        "service contract must use canonical JCS bytes"
    );
    ensure!(
        previous.service_id == current.service_id && previous.service_version == release.version,
        "service contract identity/version does not match selected Catalog release"
    );
    verify_release_lock(&previous, &lock)
        .context("rebuild and verify release lock from canonical service contract")?;

    Ok(VerifiedSignedBaseline {
        contract: previous,
        report: SignedBaselineReport {
            trust: "trusted-signed-catalog-v2",
            catalog: catalog_path,
            external_trust_store: external_trust_file.to_path_buf(),
            catalog_id: catalog.id.clone(),
            verified_key_ids: verified.key_ids().to_vec(),
            service_id: current.service_id.clone(),
            service_version: release.version.clone(),
            target: target.to_string(),
            metadata: metadata_path,
            release_lock: lock_path,
            service_contract: contract_path,
        },
    })
}

fn ensure_external_trust(catalog_directory: &Path, trust_file: &Path) -> Result<()> {
    let catalog = fs::canonicalize(catalog_directory)
        .with_context(|| format!("resolve Catalog directory {}", catalog_directory.display()))?;
    let trust = fs::canonicalize(trust_file)
        .with_context(|| format!("resolve external trust store {}", trust_file.display()))?;
    ensure!(
        !trust.starts_with(&catalog),
        "previous-trust must be operator-owned and outside previous-catalog; catalog-local trust.json is not a trust root"
    );
    Ok(())
}

fn load_external_trust(path: &Path) -> Result<CatalogTrustStore> {
    let bytes = read(path)?;
    let document: TrustDocument = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse external trust store {}", path.display()))?;
    ensure!(!document.0.is_empty(), "external trust store is empty");
    let mut trust = CatalogTrustStore::new();
    for (key_id, public_key) in document.0 {
        trust
            .insert_base64(key_id, &public_key)
            .context("validate external Ed25519 public key")?;
    }
    Ok(trust)
}

struct TrustDocument(BTreeMap<String, String>);

impl<'de> Deserialize<'de> for TrustDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = TrustDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object mapping key IDs to canonical base64 public keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut keys = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if keys.insert(key.clone(), value).is_some() {
                        return Err(de::Error::custom(format!(
                            "duplicate external trust key {key}"
                        )));
                    }
                }
                Ok(TrustDocument(keys))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
