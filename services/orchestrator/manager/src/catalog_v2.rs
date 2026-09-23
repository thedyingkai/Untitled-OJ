//! Store catalog v2 contracts and deterministic release resolution.
//!
//! This module deliberately contains no network, persistence, or runtime code.  A
//! catalog must be structurally valid before a caller verifies its signature or
//! performs any external side effect.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

pub const CATALOG_V2_SCHEMA_VERSION: u32 = 2;

/// Controls structural catalog validation only. It never establishes signer trust;
/// use [`CatalogV2::validate_trusted`] before production use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogValidationPolicy {
    pub require_signature: bool,
}

impl CatalogValidationPolicy {
    pub const fn production() -> Self {
        Self {
            require_signature: true,
        }
    }

    pub const fn development() -> Self {
        Self {
            require_signature: false,
        }
    }
}

impl Default for CatalogValidationPolicy {
    fn default() -> Self {
        Self::production()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogV2 {
    pub schema_version: u32,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub modules: Vec<CatalogModuleV2>,
    #[serde(default)]
    pub signatures: Vec<Ed25519Signature>,
}

impl CatalogV2 {
    /// Validates the typed catalog and signature envelope, but does not establish trust.
    ///
    /// Production callers must use [`Self::validate_trusted`]. Signatures cover the
    /// RFC 8785/JCS serialization of every catalog field except the top-level
    /// `signatures` array. This detached-envelope rule lets a catalog add or rotate
    /// signatures without changing the bytes signed by every other key.
    pub fn validate(&self) -> Result<(), CatalogV2Error> {
        self.validate_with_policy(CatalogValidationPolicy::production())
    }

    pub fn validate_with_policy(
        &self,
        policy: CatalogValidationPolicy,
    ) -> Result<(), CatalogV2Error> {
        if self.schema_version != CATALOG_V2_SCHEMA_VERSION {
            return Err(CatalogV2Error::UnsupportedSchema {
                expected: CATALOG_V2_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        validate_identifier("catalog.id", &self.id)?;
        if self.modules.is_empty() {
            return Err(CatalogV2Error::EmptyCatalog);
        }
        if policy.require_signature && self.signatures.is_empty() {
            return Err(CatalogV2Error::SignatureRequired);
        }

        let mut signature_keys = BTreeSet::new();
        for signature in &self.signatures {
            signature.validate()?;
            if !signature_keys.insert(signature.key_id.as_str()) {
                return Err(CatalogV2Error::DuplicateSignatureKey(
                    signature.key_id.clone(),
                ));
            }
        }

        let mut module_ids = BTreeSet::new();
        for module in &self.modules {
            module.validate()?;
            if !module_ids.insert(module.id.as_str()) {
                return Err(CatalogV2Error::DuplicateModule(module.id.clone()));
            }
        }
        for module in &self.modules {
            for release in &module.releases {
                for dependency in &release.dependencies {
                    if !module_ids.contains(dependency.module_id.as_str()) {
                        return Err(CatalogV2Error::UnknownDependency {
                            module: module.id.clone(),
                            dependency: dependency.module_id.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns the exact RFC 8785/JCS bytes covered by every catalog signature.
    pub fn signing_payload_jcs(&self) -> Result<Vec<u8>, CatalogV2Error> {
        #[derive(Serialize)]
        struct SignedCatalog<'a> {
            schema_version: u32,
            id: &'a str,
            name: &'a str,
            modules: &'a [CatalogModuleV2],
        }

        serde_json_canonicalizer::to_vec(&SignedCatalog {
            schema_version: self.schema_version,
            id: &self.id,
            name: &self.name,
            modules: &self.modules,
        })
        .map_err(|error| CatalogV2Error::CanonicalizationFailed(error.to_string()))
    }

    /// Performs strict production trust validation.
    ///
    /// Every signature must name a configured trusted key and verify over the same
    /// detached JCS payload. Unknown keys and invalid extra signatures are rejected;
    /// success therefore proves that at least one explicitly trusted signature is
    /// valid and that the complete envelope is internally consistent.
    pub fn validate_trusted(
        &self,
        trust_store: &CatalogTrustStore,
    ) -> Result<VerifiedCatalogSignatures, CatalogV2Error> {
        self.validate()?;
        if trust_store.is_empty() {
            return Err(CatalogV2Error::EmptyTrustStore);
        }

        let payload = self.signing_payload_jcs()?;
        let mut verified_key_ids = Vec::with_capacity(self.signatures.len());
        for signature in &self.signatures {
            let verifying_key = trust_store
                .keys
                .get(&signature.key_id)
                .ok_or_else(|| CatalogV2Error::UnknownSignatureKey(signature.key_id.clone()))?;
            let decoded = decode_canonical_base64(&signature.signature)
                .map_err(|_| CatalogV2Error::InvalidSignatureEncoding(signature.key_id.clone()))?;
            let bytes: [u8; 64] = decoded.try_into().map_err(|value: Vec<u8>| {
                CatalogV2Error::InvalidSignatureLength {
                    key_id: signature.key_id.clone(),
                    actual: value.len(),
                }
            })?;
            let signature_value = Signature::from_bytes(&bytes);
            verifying_key
                .verify_strict(&payload, &signature_value)
                .map_err(|_| {
                    CatalogV2Error::SignatureVerificationFailed(signature.key_id.clone())
                })?;
            verified_key_ids.push(signature.key_id.clone());
        }

        if verified_key_ids.is_empty() {
            return Err(CatalogV2Error::NoTrustedSignature);
        }
        Ok(VerifiedCatalogSignatures { verified_key_ids })
    }

    pub fn module(&self, module_id: &str) -> Option<&CatalogModuleV2> {
        self.modules.iter().find(|module| module.id == module_id)
    }

    /// Selects the highest compatible version for one module.
    pub fn select_release(
        &self,
        module_id: &str,
        version: Option<&Version>,
        channel: ReleaseChannel,
        platform: &TargetPlatform,
        orchestrator_version: &Version,
    ) -> Result<&CatalogReleaseV2, CatalogV2Error> {
        let module = self
            .module(module_id)
            .ok_or_else(|| CatalogV2Error::UnknownModule(module_id.to_string()))?;
        compatible_candidates(
            module,
            version.map(exact_requirement).as_ref(),
            channel,
            platform,
            orchestrator_version,
        )
        .into_iter()
        .next()
        .ok_or_else(|| CatalogV2Error::NoCompatibleRelease {
            module: module_id.to_string(),
            requirement: version
                .map(|version| format!("={version}"))
                .unwrap_or_else(|| "*".to_string()),
            channel,
            platform: platform.to_string(),
            orchestrator_version: orchestrator_version.clone(),
        })
    }

    /// Resolves a complete, deterministic dependency plan in dependency-first order.
    ///
    /// Resolution is backtracking rather than greedy: if two branches constrain the
    /// same dependency, a lower version satisfying both constraints is selected.
    pub fn resolve_install_plan(
        &self,
        request: &CatalogResolveRequest,
    ) -> Result<InstallPlanV2, CatalogV2Error> {
        self.validate()?;
        validate_identifier("request.module_id", &request.module_id)?;
        request.target_platform.validate()?;
        let root_requirement = request
            .version
            .as_ref()
            .map(exact_requirement)
            .unwrap_or_else(|| VersionReq::STAR);
        let root_constraint = ResolutionConstraint {
            module_id: request.module_id.clone(),
            requirement: root_requirement,
            channel: request.channel,
        };

        let root_module = self
            .module(&request.module_id)
            .ok_or_else(|| CatalogV2Error::UnknownModule(request.module_id.clone()))?;
        if compatible_candidates(
            root_module,
            Some(&root_constraint.requirement),
            root_constraint.channel,
            &request.target_platform,
            &request.orchestrator_version,
        )
        .is_empty()
        {
            return Err(CatalogV2Error::NoCompatibleRelease {
                module: request.module_id.clone(),
                requirement: root_constraint.requirement.to_string(),
                channel: request.channel,
                platform: request.target_platform.to_string(),
                orchestrator_version: request.orchestrator_version.clone(),
            });
        }

        let mut first_structural_error = None;
        let selected = solve_constraints(
            self,
            vec![root_constraint],
            BTreeMap::new(),
            &request.target_platform,
            &request.orchestrator_version,
            &mut first_structural_error,
        )?
        .ok_or_else(|| {
            first_structural_error.unwrap_or_else(|| CatalogV2Error::UnresolvableDependencies {
                module: request.module_id.clone(),
            })
        })?;
        let ordered = topological_order(&request.module_id, &selected)?;
        let root = selected
            .get(&request.module_id)
            .expect("the solved plan always contains its root");

        Ok(InstallPlanV2 {
            root: ReleaseSelectionV2 {
                module_id: request.module_id.clone(),
                version: root.version.clone(),
                channel: root.channel,
            },
            target_platform: request.target_platform.normalized(),
            releases: ordered,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogModuleV2 {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub releases: Vec<CatalogReleaseV2>,
}

impl CatalogModuleV2 {
    fn validate(&self) -> Result<(), CatalogV2Error> {
        validate_identifier("module.id", &self.id)?;
        validate_non_empty("module.name", &self.name)?;
        validate_identifier("module.kind", &self.kind)?;
        if self.releases.is_empty() {
            return Err(CatalogV2Error::ModuleHasNoReleases(self.id.clone()));
        }
        let mut releases = BTreeSet::new();
        for release in &self.releases {
            release.validate(&self.id)?;
            let identity = (release.version.clone(), release.channel);
            if !releases.insert(identity) {
                return Err(CatalogV2Error::DuplicateRelease {
                    module: self.id.clone(),
                    version: release.version.clone(),
                    channel: release.channel,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogReleaseV2 {
    pub version: Version,
    #[serde(default)]
    pub channel: ReleaseChannel,
    pub platforms: Vec<TargetPlatform>,
    pub min_orchestrator_version: Version,
    #[serde(default)]
    pub dependencies: Vec<ReleaseDependencyV2>,
    /// Runtime protocols that the signed release metadata is required to
    /// expose. Empty/absent is the compatible v2 default and means that no
    /// Topology Link may use this release as a source of active Link probes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_capabilities: Vec<RuntimeCapabilityV2>,
    pub metadata: MetadataPackageV2,
    pub oci_image: OciImageReference,
}

impl CatalogReleaseV2 {
    pub fn is_compatible_with(
        &self,
        platform: &TargetPlatform,
        orchestrator_version: &Version,
    ) -> bool {
        orchestrator_version >= &self.min_orchestrator_version
            && self
                .platforms
                .iter()
                .any(|supported| supported.supports(platform))
    }

    fn validate(&self, module_id: &str) -> Result<(), CatalogV2Error> {
        if self.channel == ReleaseChannel::Stable && !self.version.pre.is_empty() {
            return Err(CatalogV2Error::PrereleaseInStableChannel {
                module: module_id.to_string(),
                version: self.version.clone(),
            });
        }
        if self.platforms.is_empty() {
            return Err(CatalogV2Error::ReleaseHasNoPlatforms {
                module: module_id.to_string(),
                version: self.version.clone(),
            });
        }
        let mut platforms = BTreeSet::new();
        for platform in &self.platforms {
            platform.validate()?;
            if !platforms.insert(platform.normalized()) {
                return Err(CatalogV2Error::DuplicatePlatform {
                    module: module_id.to_string(),
                    version: self.version.clone(),
                    platform: platform.to_string(),
                });
            }
        }
        self.metadata.validate()?;
        let mut dependencies = BTreeSet::new();
        for dependency in &self.dependencies {
            dependency.validate()?;
            if dependency.module_id == module_id {
                return Err(CatalogV2Error::SelfDependency(module_id.to_string()));
            }
            if !dependencies.insert(dependency.module_id.as_str()) {
                return Err(CatalogV2Error::DuplicateDependency {
                    module: module_id.to_string(),
                    dependency: dependency.module_id.clone(),
                });
            }
        }
        let mut capabilities = BTreeSet::new();
        for capability in &self.runtime_capabilities {
            if !capabilities.insert(*capability) {
                return Err(CatalogV2Error::DuplicateRuntimeCapability {
                    module: module_id.to_string(),
                    capability: *capability,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeCapabilityV2 {
    LinkProbeV1,
}

impl fmt::Display for RuntimeCapabilityV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LinkProbeV1 => "link-probe-v1",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseDependencyV2 {
    pub module_id: String,
    pub requirement: VersionReq,
    #[serde(default)]
    pub channel: ReleaseChannel,
}

impl ReleaseDependencyV2 {
    fn validate(&self) -> Result<(), CatalogV2Error> {
        validate_identifier("dependency.module_id", &self.module_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MetadataPackageV2 {
    pub url: String,
    pub sha256: Sha256Digest,
}

impl MetadataPackageV2 {
    fn validate(&self) -> Result<(), CatalogV2Error> {
        validate_non_empty("metadata.url", &self.url)?;
        if self.url.chars().any(char::is_control) || self.url.chars().any(char::is_whitespace) {
            return Err(CatalogV2Error::InvalidField {
                field: "metadata.url",
                reason: "must not contain whitespace or control characters".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Ed25519Signature {
    pub key_id: String,
    #[serde(default = "ed25519_algorithm")]
    pub algorithm: String,
    /// Standard padded base64 encoding of the 64-byte Ed25519 signature.
    pub signature: String,
}

impl Ed25519Signature {
    pub fn validate(&self) -> Result<(), CatalogV2Error> {
        validate_signature_key_id(&self.key_id)?;
        if self.algorithm != "Ed25519" {
            return Err(CatalogV2Error::UnsupportedSignatureAlgorithm(
                self.algorithm.clone(),
            ));
        }
        let decoded = decode_canonical_base64(&self.signature)
            .map_err(|_| CatalogV2Error::InvalidSignatureEncoding(self.key_id.clone()))?;
        if decoded.len() != 64 {
            return Err(CatalogV2Error::InvalidSignatureLength {
                key_id: self.key_id.clone(),
                actual: decoded.len(),
            });
        }
        Ok(())
    }
}

/// Explicit catalog signing-key allowlist. Key identifiers are never derived from
/// public-key bytes: operators choose stable IDs and configure the exact mapping.
#[derive(Debug, Clone, Default)]
pub struct CatalogTrustStore {
    keys: BTreeMap<String, VerifyingKey>,
}

impl CatalogTrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn contains_key(&self, key_id: &str) -> bool {
        self.keys.contains_key(key_id)
    }

    /// Checks whether a configured key is exactly the supplied canonical
    /// padded-base64 Ed25519 public key. Catalog source registration uses this
    /// to reject attempts to reuse a trusted key ID with different key bytes.
    pub fn matches_base64(&self, key_id: &str, public_key: &str) -> Result<bool, CatalogV2Error> {
        validate_signature_key_id(key_id)?;
        let decoded = decode_canonical_base64(public_key)
            .map_err(|_| CatalogV2Error::InvalidTrustedPublicKeyEncoding(key_id.to_string()))?;
        let bytes: [u8; 32] = decoded.try_into().map_err(|value: Vec<u8>| {
            CatalogV2Error::InvalidTrustedPublicKeyLength {
                key_id: key_id.to_string(),
                actual: value.len(),
            }
        })?;
        Ok(self
            .keys
            .get(key_id)
            .is_some_and(|configured| configured.to_bytes() == bytes))
    }

    /// Adds a raw 32-byte Ed25519 verifying key under an operator-selected ID.
    pub fn insert(
        &mut self,
        key_id: impl Into<String>,
        public_key: [u8; 32],
    ) -> Result<(), CatalogV2Error> {
        let key_id = key_id.into();
        validate_signature_key_id(&key_id)?;
        if self.keys.contains_key(&key_id) {
            return Err(CatalogV2Error::DuplicateTrustedKey(key_id));
        }
        let verifying_key = VerifyingKey::from_bytes(&public_key)
            .map_err(|_| CatalogV2Error::InvalidTrustedPublicKey(key_id.clone()))?;
        self.keys.insert(key_id, verifying_key);
        Ok(())
    }

    /// Adds a standard padded-base64 encoded 32-byte Ed25519 verifying key.
    pub fn insert_base64(
        &mut self,
        key_id: impl Into<String>,
        public_key: &str,
    ) -> Result<(), CatalogV2Error> {
        let key_id = key_id.into();
        validate_signature_key_id(&key_id)?;
        if self.keys.contains_key(&key_id) {
            return Err(CatalogV2Error::DuplicateTrustedKey(key_id));
        }
        let decoded = decode_canonical_base64(public_key)
            .map_err(|_| CatalogV2Error::InvalidTrustedPublicKeyEncoding(key_id.clone()))?;
        let bytes: [u8; 32] = decoded.try_into().map_err(|value: Vec<u8>| {
            CatalogV2Error::InvalidTrustedPublicKeyLength {
                key_id: key_id.clone(),
                actual: value.len(),
            }
        })?;
        self.insert(key_id, bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCatalogSignatures {
    verified_key_ids: Vec<String>,
}

impl VerifiedCatalogSignatures {
    pub fn key_ids(&self) -> &[String] {
        &self.verified_key_ids
    }

    pub fn contains(&self, key_id: &str) -> bool {
        self.verified_key_ids.iter().any(|value| value == key_id)
    }
}

fn validate_signature_key_id(key_id: &str) -> Result<(), CatalogV2Error> {
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | ':' | '/' | '@')
        })
    {
        return Err(CatalogV2Error::InvalidSignatureKeyId(key_id.to_string()));
    }
    Ok(())
}

fn decode_canonical_base64(value: &str) -> Result<Vec<u8>, ()> {
    let decoded = STANDARD.decode(value.as_bytes()).map_err(|_| ())?;
    (STANDARD.encode(&decoded) == value)
        .then_some(decoded)
        .ok_or(())
}

fn ed25519_algorithm() -> String {
    "Ed25519".to_string()
}

#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseChannel {
    #[default]
    Stable,
    Beta,
    Nightly,
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Nightly => "nightly",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct TargetPlatform {
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl TargetPlatform {
    pub fn new(os: impl Into<String>, arch: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
            variant: None,
        }
        .normalized()
    }

    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into().trim().to_ascii_lowercase());
        self
    }

    pub fn current() -> Self {
        Self::new(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub fn normalized(&self) -> Self {
        let os = match self.os.trim().to_ascii_lowercase().as_str() {
            "win32" => "windows".to_string(),
            "darwin" => "macos".to_string(),
            value => value.to_string(),
        };
        let arch = match self.arch.trim().to_ascii_lowercase().as_str() {
            "amd64" | "x64" => "x86_64".to_string(),
            "arm64" => "aarch64".to_string(),
            value => value.to_string(),
        };
        Self {
            os,
            arch,
            variant: self
                .variant
                .as_ref()
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
        }
    }

    /// A catalog entry without a variant supports every variant of the same OS/arch.
    pub fn supports(&self, target: &TargetPlatform) -> bool {
        let supported = self.normalized();
        let target = target.normalized();
        supported.os == target.os
            && supported.arch == target.arch
            && supported
                .variant
                .as_ref()
                .is_none_or(|variant| Some(variant) == target.variant.as_ref())
    }

    fn validate(&self) -> Result<(), CatalogV2Error> {
        let platform = self.normalized();
        validate_token("platform.os", &platform.os)?;
        validate_token("platform.arch", &platform.arch)?;
        if let Some(variant) = &platform.variant {
            validate_token("platform.variant", variant)?;
        }
        Ok(())
    }
}

impl fmt::Display for TargetPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let normalized = self.normalized();
        write!(formatter, "{}/{}", normalized.os, normalized.arch)?;
        if let Some(variant) = normalized.variant {
            write!(formatter, "/{variant}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn hex(&self) -> &str {
        self.0
            .strip_prefix("sha256:")
            .expect("validated SHA-256 digest has a prefix")
    }
}

impl FromStr for Sha256Digest {
    type Err = CatalogV2Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(CatalogV2Error::InvalidSha256Digest(value.to_string()));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CatalogV2Error::InvalidSha256Digest(value.to_string()));
        }
        Ok(Self(value.to_string()))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OciImageReference {
    value: String,
    digest: Sha256Digest,
}

impl OciImageReference {
    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub fn repository(&self) -> &str {
        self.value
            .split_once('@')
            .map(|(repository, _)| repository)
            .expect("validated OCI reference contains @")
    }

    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

impl FromStr for OciImageReference {
    type Err = CatalogV2Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim() != value || value.len() > 512 || value.contains("://") {
            return Err(CatalogV2Error::InvalidOciReference(value.to_string()));
        }
        let Some((repository, digest)) = value.split_once('@') else {
            return Err(CatalogV2Error::InvalidOciReference(value.to_string()));
        };
        if repository.is_empty() || digest.contains('@') || !valid_oci_repository(repository) {
            return Err(CatalogV2Error::InvalidOciReference(value.to_string()));
        }
        let digest = digest.parse::<Sha256Digest>()?;
        Ok(Self {
            value: value.to_string(),
            digest,
        })
    }
}

impl fmt::Display for OciImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}

impl Serialize for OciImageReference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for OciImageReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogResolveRequest {
    pub module_id: String,
    pub version: Option<Version>,
    pub channel: ReleaseChannel,
    pub target_platform: TargetPlatform,
    pub orchestrator_version: Version,
}

impl CatalogResolveRequest {
    pub fn latest_stable(
        module_id: impl Into<String>,
        target_platform: TargetPlatform,
        orchestrator_version: Version,
    ) -> Self {
        Self {
            module_id: module_id.into(),
            version: None,
            channel: ReleaseChannel::Stable,
            target_platform,
            orchestrator_version,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallPlanV2 {
    pub root: ReleaseSelectionV2,
    pub target_platform: TargetPlatform,
    /// Dependency-first topological order; the root is always the last item.
    pub releases: Vec<ResolvedReleaseV2>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseSelectionV2 {
    pub module_id: String,
    pub version: Version,
    pub channel: ReleaseChannel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedReleaseV2 {
    pub module_id: String,
    pub release: CatalogReleaseV2,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CatalogV2Error {
    #[error("unsupported catalog schema {actual}; expected {expected}")]
    UnsupportedSchema { expected: u32, actual: u32 },
    #[error("catalog must contain at least one module")]
    EmptyCatalog,
    #[error("catalog must contain at least one Ed25519 signature")]
    SignatureRequired,
    #[error("invalid {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("duplicate signature for key {0}")]
    DuplicateSignatureKey(String),
    #[error("invalid signature key id {0}")]
    InvalidSignatureKeyId(String),
    #[error("unsupported signature algorithm {0}; expected Ed25519")]
    UnsupportedSignatureAlgorithm(String),
    #[error("signature for key {0} is not standard base64")]
    InvalidSignatureEncoding(String),
    #[error("signature for key {key_id} is {actual} bytes; expected 64")]
    InvalidSignatureLength { key_id: String, actual: usize },
    #[error("catalog trust store must contain at least one Ed25519 public key")]
    EmptyTrustStore,
    #[error("duplicate trusted catalog key {0}")]
    DuplicateTrustedKey(String),
    #[error("trusted catalog key {0} is not standard base64")]
    InvalidTrustedPublicKeyEncoding(String),
    #[error("trusted catalog key {key_id} is {actual} bytes; expected 32")]
    InvalidTrustedPublicKeyLength { key_id: String, actual: usize },
    #[error("trusted catalog key {0} is not a valid Ed25519 public key")]
    InvalidTrustedPublicKey(String),
    #[error("catalog signature references unknown trusted key {0}")]
    UnknownSignatureKey(String),
    #[error("catalog signature verification failed for key {0}")]
    SignatureVerificationFailed(String),
    #[error("catalog must contain at least one valid trusted signature")]
    NoTrustedSignature,
    #[error("catalog RFC 8785 canonicalization failed: {0}")]
    CanonicalizationFailed(String),
    #[error("duplicate module {0}")]
    DuplicateModule(String),
    #[error("module {0} has no releases")]
    ModuleHasNoReleases(String),
    #[error("duplicate release {module} {version} in {channel} channel")]
    DuplicateRelease {
        module: String,
        version: Version,
        channel: ReleaseChannel,
    },
    #[error("stable channel release {module} {version} must not be a prerelease")]
    PrereleaseInStableChannel { module: String, version: Version },
    #[error("release {module} {version} has no supported platforms")]
    ReleaseHasNoPlatforms { module: String, version: Version },
    #[error("release {module} {version} repeats platform {platform}")]
    DuplicatePlatform {
        module: String,
        version: Version,
        platform: String,
    },
    #[error("module {0} depends on itself")]
    SelfDependency(String),
    #[error("module {module} repeats dependency {dependency}")]
    DuplicateDependency { module: String, dependency: String },
    #[error("module {module} repeats runtime capability {capability}")]
    DuplicateRuntimeCapability {
        module: String,
        capability: RuntimeCapabilityV2,
    },
    #[error("module {module} references unknown dependency {dependency}")]
    UnknownDependency { module: String, dependency: String },
    #[error("unknown module {0}")]
    UnknownModule(String),
    #[error(
        "no compatible {module} release matching {requirement} in {channel} for {platform} on orchestrator {orchestrator_version}"
    )]
    NoCompatibleRelease {
        module: String,
        requirement: String,
        channel: ReleaseChannel,
        platform: String,
        orchestrator_version: Version,
    },
    #[error("dependencies for {module} have no compatible version assignment")]
    UnresolvableDependencies { module: String },
    #[error("dependency cycle detected: {cycle}")]
    DependencyCycle { cycle: String },
    #[error("invalid canonical SHA-256 digest {0}")]
    InvalidSha256Digest(String),
    #[error(
        "invalid immutable OCI image reference {0}; expected repository@sha256:<64 lowercase hex>"
    )]
    InvalidOciReference(String),
}

#[derive(Debug, Clone)]
struct ResolutionConstraint {
    module_id: String,
    requirement: VersionReq,
    channel: ReleaseChannel,
}

fn solve_constraints<'a>(
    catalog: &'a CatalogV2,
    mut pending: Vec<ResolutionConstraint>,
    selected: BTreeMap<String, &'a CatalogReleaseV2>,
    platform: &TargetPlatform,
    orchestrator_version: &Version,
    first_structural_error: &mut Option<CatalogV2Error>,
) -> Result<Option<BTreeMap<String, &'a CatalogReleaseV2>>, CatalogV2Error> {
    let Some(constraint) = pending.pop() else {
        if let Err(error) = topological_order("", &selected) {
            first_structural_error.get_or_insert(error);
            return Ok(None);
        }
        return Ok(Some(selected));
    };

    if let Some(release) = selected.get(&constraint.module_id) {
        if constraint.channel != release.channel
            || !constraint.requirement.matches(&release.version)
        {
            return Ok(None);
        }
        return solve_constraints(
            catalog,
            pending,
            selected,
            platform,
            orchestrator_version,
            first_structural_error,
        );
    }

    let Some(module) = catalog.module(&constraint.module_id) else {
        return Ok(None);
    };
    for release in compatible_candidates(
        module,
        Some(&constraint.requirement),
        constraint.channel,
        platform,
        orchestrator_version,
    ) {
        let mut branch_selected = selected.clone();
        branch_selected.insert(constraint.module_id.clone(), release);
        let mut branch_pending = pending.clone();
        let mut dependencies = release.dependencies.iter().collect::<Vec<_>>();
        dependencies.sort_by(|left, right| left.module_id.cmp(&right.module_id));
        for dependency in dependencies.into_iter().rev() {
            branch_pending.push(ResolutionConstraint {
                module_id: dependency.module_id.clone(),
                requirement: dependency.requirement.clone(),
                channel: dependency.channel,
            });
        }
        if let Some(solution) = solve_constraints(
            catalog,
            branch_pending,
            branch_selected,
            platform,
            orchestrator_version,
            first_structural_error,
        )? {
            return Ok(Some(solution));
        }
    }
    Ok(None)
}

fn compatible_candidates<'a>(
    module: &'a CatalogModuleV2,
    requirement: Option<&VersionReq>,
    channel: ReleaseChannel,
    platform: &TargetPlatform,
    orchestrator_version: &Version,
) -> Vec<&'a CatalogReleaseV2> {
    let mut candidates = module
        .releases
        .iter()
        .filter(|release| release.channel == channel)
        .filter(|release| {
            requirement.is_none_or(|requirement| requirement.matches(&release.version))
        })
        .filter(|release| release.is_compatible_with(platform, orchestrator_version))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.version.cmp(&left.version));
    candidates
}

fn exact_requirement(version: &Version) -> VersionReq {
    VersionReq::parse(&format!("={version}")).expect("a semver Version is always a valid exact req")
}

fn topological_order(
    root: &str,
    selected: &BTreeMap<String, &CatalogReleaseV2>,
) -> Result<Vec<ResolvedReleaseV2>, CatalogV2Error> {
    let mut marks = BTreeMap::<String, VisitMark>::new();
    let mut stack = Vec::new();
    let mut ordered = Vec::new();

    if root.is_empty() {
        for module_id in selected.keys() {
            visit_selected(module_id, selected, &mut marks, &mut stack, &mut ordered)?;
        }
    } else {
        visit_selected(root, selected, &mut marks, &mut stack, &mut ordered)?;
    }
    Ok(ordered)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitMark {
    Visiting,
    Visited,
}

fn visit_selected(
    module_id: &str,
    selected: &BTreeMap<String, &CatalogReleaseV2>,
    marks: &mut BTreeMap<String, VisitMark>,
    stack: &mut Vec<String>,
    ordered: &mut Vec<ResolvedReleaseV2>,
) -> Result<(), CatalogV2Error> {
    match marks.get(module_id) {
        Some(VisitMark::Visited) => return Ok(()),
        Some(VisitMark::Visiting) => {
            let cycle_start = stack.iter().position(|item| item == module_id).unwrap_or(0);
            let mut cycle = stack[cycle_start..].to_vec();
            cycle.push(module_id.to_string());
            return Err(CatalogV2Error::DependencyCycle {
                cycle: cycle.join(" -> "),
            });
        }
        None => {}
    }
    let release =
        selected
            .get(module_id)
            .ok_or_else(|| CatalogV2Error::UnresolvableDependencies {
                module: module_id.to_string(),
            })?;
    marks.insert(module_id.to_string(), VisitMark::Visiting);
    stack.push(module_id.to_string());
    let mut dependencies = release.dependencies.iter().collect::<Vec<_>>();
    dependencies.sort_by(|left, right| left.module_id.cmp(&right.module_id));
    for dependency in dependencies {
        visit_selected(&dependency.module_id, selected, marks, stack, ordered)?;
    }
    stack.pop();
    marks.insert(module_id.to_string(), VisitMark::Visited);
    ordered.push(ResolvedReleaseV2 {
        module_id: module_id.to_string(),
        release: (*release).clone(),
    });
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), CatalogV2Error> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(CatalogV2Error::InvalidField {
            field,
            reason: "must be a lowercase identifier using a-z, 0-9, '.', '_' or '-'".to_string(),
        });
    }
    Ok(())
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), CatalogV2Error> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(CatalogV2Error::InvalidField {
            field,
            reason: "must be non-empty and have no surrounding whitespace".to_string(),
        });
    }
    Ok(())
}

fn validate_token(field: &'static str, value: &str) -> Result<(), CatalogV2Error> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
    {
        return Err(CatalogV2Error::InvalidField {
            field,
            reason: "must be a lowercase platform token".to_string(),
        });
    }
    Ok(())
}

fn valid_oci_repository(repository: &str) -> bool {
    if repository.len() > 255
        || repository.starts_with('/')
        || repository.ends_with('/')
        || repository.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return false;
    }
    let components = repository.split('/').collect::<Vec<_>>();
    components.iter().enumerate().all(|(index, component)| {
        if component.is_empty() {
            return false;
        }
        if index == 0 && component.contains(':') {
            if components.len() == 1 {
                return false;
            }
            let Some((host, port)) = component.rsplit_once(':') else {
                return false;
            };
            return valid_repository_component(host, false)
                && !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port > 0);
        }
        valid_repository_component(component, true)
    })
}

fn valid_repository_component(component: &str, allow_underscore: bool) -> bool {
    let bytes = component.as_bytes();
    if !bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() {
        if bytes[index].is_ascii_lowercase() || bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        match bytes[index] {
            b'.' => index += 1,
            b'-' => {
                while index < bytes.len() && bytes[index] == b'-' {
                    index += 1;
                }
            }
            b'_' if allow_underscore => {
                index += 1;
                if index < bytes.len() && bytes[index] == b'_' {
                    index += 1;
                }
            }
            _ => return false,
        }
        if !bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return false;
        }
    }
    true
}
