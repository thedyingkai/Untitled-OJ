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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    use ojos_service::{
        ApiOperationV3, ApiSurfaceV3, EventsContractV1, HealthSource, RuntimeSource,
        seal::{RESOLVED_ARTIFACTS_SCHEMA_VERSION, ResolvedArtifactV1, ResolvedArtifactsV1, seal},
    };
    use orchestrator_core::{
        RELEASE_PLATFORM_SCHEMA_VERSION, ReleaseArtifactSubjectV1, ReleasePlatformContractV1,
        ServiceReleaseContract,
    };
    use orchestrator_manager::catalog_v2::{
        CatalogModuleV2, CatalogReleaseV2, Ed25519Signature, MetadataPackageV2, OciImageReference,
    };
    use serde_json::json;

    fn sha(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn contract(version: &str, api_major: u64) -> ServiceContractV3 {
        ServiceContractV3 {
            schema_version: ojos_service::SERVICE_CONTRACT_SCHEMA_VERSION.to_string(),
            compiler_version: ojos_service::COMPILER_VERSION.to_string(),
            service_id: "contest-service".to_string(),
            service_version: Version::parse(version).unwrap(),
            display_name: "Contest".to_string(),
            source_digest: sha('1'),
            runtime: RuntimeSource {
                profile: "standard-container-v1".to_string(),
                artifact: "runtime".to_string(),
                http_port: 8080,
                health: HealthSource {
                    path: "/healthz".to_string(),
                },
                volumes: Vec::new(),
            },
            api_surfaces: vec![ApiSurfaceV3 {
                api_id: "contest.api".to_string(),
                version: Version::new(api_major, 0, 0),
                document: "api/openapi.yaml".to_string(),
                document_digest: sha('2'),
            }],
            operations: vec![ApiOperationV3 {
                api_id: "contest.api".to_string(),
                api_version: Version::new(api_major, 0, 0),
                operation_id: "listContests".to_string(),
                provider_path: "/contests".to_string(),
                method: "GET".to_string(),
                audience: "user".to_string(),
                auth: "required".to_string(),
                permission: None,
                permission_scope: None,
                parameters: Vec::new(),
                request_body: None,
                responses: Vec::new(),
            }],
            api_requirements: Vec::new(),
            package_requirements: Vec::new(),
            resource_claims: Vec::new(),
            migrations: Vec::new(),
            events: EventsContractV1::default(),
            permissions: Vec::new(),
            permission_references: Vec::new(),
            exposures: Vec::new(),
            routes: Vec::new(),
            frontends: Vec::new(),
            config_schema: None,
        }
    }

    fn lock(contract: &ServiceContractV3) -> ReleaseLockV1 {
        let bytes = contract_bytes(contract).unwrap();
        let mut resolved = ResolvedArtifactsV1 {
            schema_version: RESOLVED_ARTIFACTS_SCHEMA_VERSION.to_string(),
            artifacts: BTreeMap::from([
                (
                    "contract".to_string(),
                    ResolvedArtifactV1 {
                        media_type: "application/json".to_string(),
                        digest: digest(&bytes),
                        size: bytes.len() as u64,
                        reference: Some("https://artifacts.example/contract".to_string()),
                    },
                ),
                (
                    "runtime".to_string(),
                    ResolvedArtifactV1 {
                        media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                        digest: sha('5'),
                        size: 10,
                        reference: Some(format!("registry.example/contest@{}", sha('5'))),
                    },
                ),
                ("sbom".to_string(), blob(sha('6'))),
                ("provenance".to_string(), blob(sha('7'))),
                ("events".to_string(), blob(sha('8'))),
                ("openapi.contest.api".to_string(), blob(sha('2'))),
            ]),
        };
        for requirement in ojos_service::seal::artifact_requirements(contract).unwrap() {
            let artifact = resolved.artifacts.get_mut(&requirement.slot).unwrap();
            if let Some(expected) = requirement.expected_digest {
                artifact.digest = expected;
            }
            if let Some(expected) = requirement.expected_size {
                artifact.size = expected;
            }
        }
        seal(contract, &resolved).unwrap()
    }

    fn blob(digest_value: String) -> ResolvedArtifactV1 {
        ResolvedArtifactV1 {
            media_type: "application/octet-stream".to_string(),
            size: 10,
            reference: Some(format!(
                "https://artifacts.example/{}/subject",
                digest_value.trim_start_matches("sha256:")
            )),
            digest: digest_value,
        }
    }

    fn metadata(contract: &ServiceContractV3, lock: &ReleaseLockV1) -> Vec<u8> {
        let subjects = lock
            .artifacts
            .iter()
            .map(|(slot, artifact)| ReleaseArtifactSubjectV1 {
                slot: slot.clone(),
                roles: lock
                    .bindings
                    .iter()
                    .filter(|binding| binding.slot == *slot)
                    .map(|binding| binding.role.clone())
                    .collect(),
                media_type: artifact.media_type.clone(),
                digest: artifact.digest.clone(),
                size: artifact.size,
                reference: artifact.reference.clone(),
            })
            .collect();
        let platform = ReleasePlatformContractV1 {
            schema_version: RELEASE_PLATFORM_SCHEMA_VERSION.to_string(),
            contract_digest: lock.contract_digest.clone(),
            source_digest: contract.source_digest.clone(),
            release_lock_digest: release_lock_digest(lock).unwrap(),
            artifact_subjects: subjects,
            package_requirements: Vec::new(),
            resource_claims: Vec::new(),
            runtime_volumes: Vec::new(),
            config_schema: None,
            contribution: Default::default(),
        };
        let value = json!({
            "schema_version": 2,
            "service_name": contract.service_id,
            "version": contract.service_version,
            "description": "fixture",
            "service_type": "backend-api",
            "source": {"kind":"url", "url":"https://example.invalid/release", "checksum":""},
            "runtime": {"kind":"image", "image":format!("registry.example/contest@{}", sha('5')), "binary":"", "system_service":"", "env":{}},
            "frontend": {"enabled":false, "route_prefix":"", "remote_entry":"", "menu_items":[]},
            "backend": {"protocol":"http", "port":8080, "health_path":"/healthz"},
            "migrations": [], "permissions": [], "routes": [], "apis": [], "redis": [],
            "storage": [], "dependencies": [], "required_apis": [],
            "service_identity":{"service_name":contract.service_id,"allowed_apis":[]},
            "config_schema":{}, "secrets":[], "observability":{"metrics":true,"jaeger":true},
            "provides":{"apis":[],"events":[]}, "requires":{"apis":[],"events":[]},
            "events":{"publishes":[],"subscribes":[]},
            "runtime_contract":{
                "id":"standard-container-v1",
                "sha256":orchestrator_core::STANDARD_CONTAINER_RUNTIME_SHA256,
                "binding_directory":"/run/ojos/service", "identity_mode":"workload",
                "credential_delivery":"file", "restart_on_change":false, "env_projection":{}
            },
            "platform": platform
        });
        let strict = ServiceReleaseContract::from_json_value(value).unwrap();
        pretty(&strict.to_json_value().unwrap())
    }

    fn fixture(root: &Path, releases: &[(&str, u64)], key_seed: u8) -> PathBuf {
        let catalog_dir = root.join("catalog");
        fs::create_dir_all(catalog_dir.join("metadata")).unwrap();
        let signing = SigningKey::from_bytes(&[key_seed; 32]);
        let mut catalog_releases = Vec::new();
        for (version, api_major) in releases {
            let contract = contract(version, *api_major);
            let lock = lock(&contract);
            let stem = format!("{}-{}", contract.service_id, contract.service_version);
            let metadata_bytes = metadata(&contract, &lock);
            fs::write(
                catalog_dir
                    .join("metadata")
                    .join(format!("{stem}.release.json")),
                &metadata_bytes,
            )
            .unwrap();
            fs::write(
                catalog_dir
                    .join("metadata")
                    .join(format!("{stem}.release.lock.json")),
                release_lock_bytes(&lock).unwrap(),
            )
            .unwrap();
            fs::write(
                catalog_dir
                    .join("metadata")
                    .join(format!("{stem}.service.contract.json")),
                contract_bytes(&contract).unwrap(),
            )
            .unwrap();
            catalog_releases.push(CatalogReleaseV2 {
                version: contract.service_version,
                channel: ReleaseChannel::Stable,
                platforms: vec![TargetPlatform::new("linux", "x86_64")],
                min_orchestrator_version: Version::new(1, 0, 0),
                dependencies: Vec::new(),
                runtime_capabilities: Vec::new(),
                metadata: MetadataPackageV2 {
                    url: "https://example.invalid/release".to_string(),
                    sha256: digest(&metadata_bytes).parse().unwrap(),
                },
                oci_image: format!("registry.example/contest@{}", sha('5'))
                    .parse::<OciImageReference>()
                    .unwrap(),
            });
        }
        let mut catalog = CatalogV2 {
            schema_version: 2,
            id: "fixture".to_string(),
            name: "fixture".to_string(),
            modules: vec![CatalogModuleV2 {
                id: "contest-service".to_string(),
                name: "Contest".to_string(),
                description: "fixture".to_string(),
                kind: "backend-api".to_string(),
                tags: Vec::new(),
                releases: catalog_releases,
            }],
            signatures: Vec::new(),
        };
        let signature = signing.sign(&catalog.signing_payload_jcs().unwrap());
        catalog.signatures.push(Ed25519Signature {
            key_id: "release-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(signature.to_bytes()),
        });
        fs::write(catalog_dir.join("catalog.json"), pretty(&catalog)).unwrap();
        let trust = root.join("operator-trust.json");
        fs::write(
            &trust,
            pretty(&json!({"release-key": STANDARD.encode(signing.verifying_key().to_bytes())})),
        )
        .unwrap();
        trust
    }

    fn pretty(value: &impl Serialize) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(value).unwrap();
        bytes.push(b'\n');
        bytes
    }

    fn resign_catalog(path: &Path, seed: u8) {
        let mut catalog: CatalogV2 = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let signing = SigningKey::from_bytes(&[seed; 32]);
        catalog.signatures.clear();
        catalog.signatures.push(Ed25519Signature {
            key_id: "release-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(
                signing
                    .sign(&catalog.signing_payload_jcs().unwrap())
                    .to_bytes(),
            ),
        });
        fs::write(path, pretty(&catalog)).unwrap();
    }

    #[test]
    fn selects_highest_prior_stable_and_proves_chain() {
        let temp = tempfile::tempdir().unwrap();
        let trust = fixture(
            temp.path(),
            &[("1.0.0", 1), ("1.2.0", 1), ("2.0.0", 2), ("3.0.0", 3)],
            7,
        );
        let current = contract("2.0.0", 2);
        let verified = load_signed_baseline(
            &temp.path().join("catalog"),
            &trust,
            &current,
            &TargetPlatform::new("linux", "x86_64"),
        )
        .unwrap();
        assert_eq!(verified.contract.service_version, Version::new(1, 2, 0));
        assert_eq!(verified.report.trust, "trusted-signed-catalog-v2");
    }

    #[test]
    fn rejects_no_prior_and_catalog_local_or_replaced_trust() {
        let temp = tempfile::tempdir().unwrap();
        let trust = fixture(temp.path(), &[("2.0.0", 2), ("3.0.0", 3)], 7);
        let current = contract("2.0.0", 2);
        assert!(
            load_signed_baseline(
                &temp.path().join("catalog"),
                &trust,
                &current,
                &TargetPlatform::new("linux", "x86_64")
            )
            .is_err()
        );

        let local = temp.path().join("catalog/trust.json");
        fs::write(&local, fs::read(&trust).unwrap()).unwrap();
        assert!(
            load_signed_baseline(
                &temp.path().join("catalog"),
                &local,
                &contract("4.0.0", 4),
                &TargetPlatform::new("linux", "x86_64")
            )
            .is_err()
        );

        let attacker = SigningKey::from_bytes(&[9; 32]);
        let catalog_path = temp.path().join("catalog/catalog.json");
        resign_catalog(&catalog_path, 9);
        fs::write(
            &local,
            pretty(&json!({"release-key": STANDARD.encode(attacker.verifying_key().to_bytes())})),
        )
        .unwrap();
        assert!(
            load_signed_baseline(
                &temp.path().join("catalog"),
                &trust,
                &contract("4.0.0", 4),
                &TargetPlatform::new("linux", "x86_64")
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_extra_signature_even_with_valid_trusted_signature() {
        let temp = tempfile::tempdir().unwrap();
        let trust = fixture(temp.path(), &[("1.0.0", 1)], 7);
        let path = temp.path().join("catalog/catalog.json");
        let mut catalog: CatalogV2 = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let attacker = SigningKey::from_bytes(&[9; 32]);
        catalog.signatures.push(Ed25519Signature {
            key_id: "unknown-attacker".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(
                attacker
                    .sign(&catalog.signing_payload_jcs().unwrap())
                    .to_bytes(),
            ),
        });
        fs::write(path, pretty(&catalog)).unwrap();
        assert!(
            load_signed_baseline(
                &temp.path().join("catalog"),
                &trust,
                &contract("1.1.0", 1),
                &TargetPlatform::new("linux", "x86_64")
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_catalog_metadata_lock_and_contract_tampering() {
        for target in [
            "catalog.json",
            "metadata/contest-service-1.0.0.release.json",
            "metadata/contest-service-1.0.0.release.lock.json",
            "metadata/contest-service-1.0.0.service.contract.json",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let trust = fixture(temp.path(), &[("1.0.0", 1)], 7);
            let path = temp.path().join("catalog").join(target);
            let mut bytes = fs::read(&path).unwrap();
            if target == "catalog.json" {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                value["name"] = json!("tampered signed payload");
                bytes = pretty(&value);
            } else {
                bytes.push(b' ');
            }
            fs::write(&path, bytes).unwrap();
            assert!(
                load_signed_baseline(
                    &temp.path().join("catalog"),
                    &trust,
                    &contract("1.1.0", 1),
                    &TargetPlatform::new("linux", "x86_64")
                )
                .is_err(),
                "tamper accepted for {target}"
            );
        }
    }

    #[test]
    fn detects_identity_mismatch_and_compatibility_major_policy() {
        let temp = tempfile::tempdir().unwrap();
        let trust = fixture(temp.path(), &[("1.0.0", 1)], 7);
        let verified = load_signed_baseline(
            &temp.path().join("catalog"),
            &trust,
            &contract("1.1.0", 1),
            &TargetPlatform::new("linux", "x86_64"),
        )
        .unwrap();
        let mut breaking_minor = contract("1.1.0", 1);
        breaking_minor.operations.clear();
        assert!(
            !ojos_service::compatibility::compare(&verified.contract, &breaking_minor)
                .unwrap()
                .compatible
        );
        let mut breaking_major = contract("2.0.0", 2);
        breaking_major.operations.clear();
        assert!(
            ojos_service::compatibility::compare(&verified.contract, &breaking_major)
                .unwrap()
                .compatible
        );

        let contract_path = temp
            .path()
            .join("catalog/metadata/contest-service-1.0.0.service.contract.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&contract_path).unwrap()).unwrap();
        value["serviceId"] = json!("wrong-service");
        fs::write(
            &contract_path,
            serde_json_canonicalizer::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(
            load_signed_baseline(
                &temp.path().join("catalog"),
                &trust,
                &contract("1.1.0", 1),
                &TargetPlatform::new("linux", "x86_64")
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_signed_metadata_service_and_version_mismatch() {
        for (field, mismatch) in [
            ("service_name", json!("wrong-service")),
            ("version", json!("9.9.9")),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let trust = fixture(temp.path(), &[("1.0.0", 1)], 7);
            let metadata_path = temp
                .path()
                .join("catalog/metadata/contest-service-1.0.0.release.json");
            let mut metadata: serde_json::Value =
                serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
            metadata[field] = mismatch;
            let bytes = pretty(&metadata);
            fs::write(&metadata_path, &bytes).unwrap();

            let catalog_path = temp.path().join("catalog/catalog.json");
            let mut catalog: CatalogV2 =
                serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
            catalog.modules[0].releases[0].metadata.sha256 = digest(&bytes).parse().unwrap();
            fs::write(&catalog_path, pretty(&catalog)).unwrap();
            resign_catalog(&catalog_path, 7);

            assert!(
                load_signed_baseline(
                    &temp.path().join("catalog"),
                    &trust,
                    &contract("1.1.0", 1),
                    &TargetPlatform::new("linux", "x86_64")
                )
                .is_err(),
                "signed metadata {field} mismatch was accepted"
            );
        }
    }
}
