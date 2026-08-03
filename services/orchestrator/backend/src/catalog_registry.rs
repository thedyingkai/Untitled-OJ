//! Durable Catalog v2 source registry and trusted release resolution.
//!
//! The registry owns network/local-file loading and signature verification. It
//! never performs runtime work. Callers receive an immutable, dependency-first
//! plan and exact metadata bytes whose SHA-256 has been checked again.

use crate::durable::{DurableError, DurableStore};
use orchestrator_legacy::{
    external_release_import_from_yaml, release_supports_link_probe_v1, resolve_outbound_redirect,
};
use orchestrator_manager::catalog_v2::{
    CatalogResolveRequest, CatalogTrustStore, CatalogV2, CatalogV2Error, InstallPlanV2,
    OciImageReference as CatalogOciImageReference, ReleaseChannel, ResolvedReleaseV2,
    RuntimeCapabilityV2, TargetPlatform,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

const REGISTRY_NAMESPACE: &str = "catalog-v2";
const REGISTRY_STATE_KEY: &str = "registry-state-v1";
// Read-only migration keys used by pre-v1 snapshots. New writes must use the
// single registry snapshot so a trust anchor and its verified source cannot
// be torn apart by a crash between two commits.
const SOURCES_KEY: &str = "sources";
const TRUST_KEYS_KEY: &str = "trust-keys";
const TRUST_KEYS_ENV: &str = "ORCHESTRATOR_CATALOG_TRUST_KEYS";
const SOURCES_ENV: &str = "ORCHESTRATOR_CATALOG_SOURCES";
const DEFAULT_GITHUB_TOKEN_ENV: &str = "GITHUB_TOKEN";
const FALLBACK_GITHUB_TOKEN_ENV: &str = "OJOS_GITHUB_TOKEN";
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_REDIRECTS: u8 = 5;
const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 200;
const USER_AGENT: &str = "ojos-orchestrator-catalog-v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogSource {
    pub id: String,
    pub url: String,
    pub required_key_id: String,
    #[serde(default)]
    pub auth_secret_ref: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Optional verified OCI image-layout mirrors. The key is the exact
    /// repository@digest reference and the value is a repository-local path.
    #[serde(default)]
    pub offline_oci_layouts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersistedCatalogRegistry {
    #[serde(default)]
    trust_keys: BTreeMap<String, String>,
    #[serde(default)]
    sources: Vec<CatalogSource>,
}

/// Write-only registration shape. `public_key` is accepted so a Desktop user
/// can establish the first local trust anchor; source reads never echo it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogSourceRegistration {
    pub id: String,
    pub url: String,
    pub required_key_id: String,
    #[serde(default)]
    pub auth_secret_ref: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub offline_oci_layouts: BTreeMap<String, String>,
    #[serde(default)]
    pub public_key: Option<String>,
}

impl CatalogSourceRegistration {
    fn into_parts(self) -> (CatalogSource, Option<String>) {
        (
            CatalogSource {
                id: self.id,
                url: self.url,
                required_key_id: self.required_key_id,
                auth_secret_ref: self.auth_secret_ref,
                enabled: self.enabled,
                offline_oci_layouts: self.offline_oci_layouts,
            },
            self.public_key,
        )
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PackageQuery {
    pub search: Option<String>,
    pub channel: Option<ReleaseChannel>,
    pub platform: Option<TargetPlatform>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct CatalogPackageItem {
    pub source_id: String,
    pub catalog_id: String,
    pub module_id: String,
    pub name: String,
    pub description: String,
    pub kind: String,
    pub tags: Vec<String>,
    pub version: Version,
    pub channel: ReleaseChannel,
    pub platforms: Vec<TargetPlatform>,
    pub min_orchestrator_version: Version,
    pub runtime_capabilities: Vec<RuntimeCapabilityV2>,
    pub metadata_sha256: String,
    pub oci_image: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PackagePage {
    pub items: Vec<CatalogPackageItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct CatalogSourcePage {
    pub items: Vec<CatalogSource>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ResolvedCatalogPlan {
    pub source_id: String,
    pub catalog_id: String,
    pub verified_key_ids: Vec<String>,
    pub plan: InstallPlanV2,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedReleaseDocument {
    pub selection: ResolvedReleaseV2,
    pub source_url: String,
    pub checksum: String,
    pub bytes: Vec<u8>,
    pub offline_oci_layout: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogRegistry {
    repo_root: PathBuf,
    trust_store: Arc<RwLock<CatalogTrustStore>>,
    bootstrap_trust_keys: BTreeMap<String, String>,
    orchestrator_version: Version,
    bootstrap_sources: Vec<CatalogSource>,
    allow_dynamic_trust: bool,
    allow_empty_sources: bool,
    source_mutation_lock: Arc<Mutex<()>>,
}

impl CatalogRegistry {
    pub(crate) fn from_env(repo_root: &Path) -> Result<Option<Self>, CatalogRegistryError> {
        Self::from_env_values(
            repo_root,
            std::env::var(TRUST_KEYS_ENV).ok(),
            std::env::var(SOURCES_ENV).ok(),
        )
    }

    fn from_env_values(
        repo_root: &Path,
        trust_json: Option<String>,
        sources_json: Option<String>,
    ) -> Result<Option<Self>, CatalogRegistryError> {
        let trust_json = trust_json.filter(|value| !value.trim().is_empty());
        let sources_json = sources_json.filter(|value| !value.trim().is_empty());
        if trust_json.is_none() && sources_json.is_none() {
            return Ok(None);
        }
        let trust_json = trust_json.ok_or_else(|| {
            CatalogRegistryError::configuration(format!(
                "{TRUST_KEYS_ENV} is required when {SOURCES_ENV} is configured"
            ))
        })?;
        let sources_json = sources_json.ok_or_else(|| {
            CatalogRegistryError::configuration(format!(
                "{SOURCES_ENV} is required when {TRUST_KEYS_ENV} is configured"
            ))
        })?;
        let keys: BTreeMap<String, String> = serde_json::from_str(&trust_json).map_err(|error| {
            CatalogRegistryError::configuration(format!(
                "{TRUST_KEYS_ENV} must be a JSON object of key_id to padded-base64 Ed25519 public key: {error}"
            ))
        })?;
        let sources: Vec<CatalogSource> = serde_json::from_str(&sources_json).map_err(|error| {
            CatalogRegistryError::configuration(format!(
                "{SOURCES_ENV} must be a JSON array of CatalogSource objects: {error}"
            ))
        })?;
        if keys.is_empty() {
            return Err(CatalogRegistryError::configuration(format!(
                "{TRUST_KEYS_ENV} must not be empty"
            )));
        }
        if sources.is_empty() {
            return Err(CatalogRegistryError::configuration(format!(
                "{SOURCES_ENV} must not be empty"
            )));
        }
        let mut trust_store = CatalogTrustStore::new();
        for (key_id, public_key) in &keys {
            trust_store
                .insert_base64(key_id.clone(), public_key)
                .map_err(CatalogRegistryError::catalog_trust)?;
        }
        Self::new_internal(repo_root, trust_store, keys, sources, false, false).map(Some)
    }

    #[cfg(test)]
    pub(crate) fn new(
        repo_root: &Path,
        trust_store: CatalogTrustStore,
        bootstrap_sources: Vec<CatalogSource>,
    ) -> Result<Self, CatalogRegistryError> {
        if trust_store.is_empty() {
            return Err(CatalogRegistryError::configuration(
                "catalog trust store must contain at least one explicitly configured Ed25519 key",
            ));
        }
        Self::new_internal(
            repo_root,
            trust_store,
            BTreeMap::new(),
            bootstrap_sources,
            false,
            false,
        )
    }

    /// Desktop starts with a durable but empty trust registry. The first
    /// `catalog.register` request must provide an Ed25519 public key and a
    /// catalog signed by that key before either value becomes active.
    pub(crate) fn desktop(repo_root: &Path) -> Result<Self, CatalogRegistryError> {
        Self::new_internal(
            repo_root,
            CatalogTrustStore::new(),
            BTreeMap::new(),
            Vec::new(),
            true,
            true,
        )
    }

    fn new_internal(
        repo_root: &Path,
        trust_store: CatalogTrustStore,
        bootstrap_trust_keys: BTreeMap<String, String>,
        bootstrap_sources: Vec<CatalogSource>,
        allow_dynamic_trust: bool,
        allow_empty_sources: bool,
    ) -> Result<Self, CatalogRegistryError> {
        let registry = Self {
            repo_root: repo_root.to_path_buf(),
            trust_store: Arc::new(RwLock::new(trust_store)),
            bootstrap_trust_keys,
            orchestrator_version: Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
                CatalogRegistryError::configuration(format!(
                    "orchestrator package version is not semver: {error}"
                ))
            })?,
            bootstrap_sources,
            allow_dynamic_trust,
            allow_empty_sources,
            source_mutation_lock: Arc::new(Mutex::new(())),
        };
        for source in &registry.bootstrap_sources {
            registry.validate_source_configuration(source)?;
        }
        Ok(registry)
    }

    /// Verifies every configured bootstrap source before it becomes visible.
    /// Existing API-managed sources remain durable and are not silently deleted.
    pub(crate) fn bootstrap(&self, storage: &DurableStore) -> Result<(), CatalogRegistryError> {
        let _guard = self.source_mutation_lock.lock().map_err(|_| {
            CatalogRegistryError::storage("catalog source mutation lock is poisoned")
        })?;
        let mut state = self.load_registry_state_unlocked(storage)?;
        let mut trust_store = self.trust_snapshot()?;
        self.merge_persisted_trust(&mut trust_store, &state.trust_keys)?;
        for (key_id, public_key) in &self.bootstrap_trust_keys {
            if state
                .trust_keys
                .get(key_id)
                .is_some_and(|existing| existing != public_key)
            {
                return Err(CatalogRegistryError::configuration(format!(
                    "durable Catalog trust key {key_id} conflicts with the configured bootstrap key"
                )));
            }
            if trust_store.contains_key(key_id) {
                if !trust_store
                    .matches_base64(key_id, public_key)
                    .map_err(CatalogRegistryError::catalog_trust)?
                {
                    return Err(CatalogRegistryError::configuration(format!(
                        "durable Catalog trust key {key_id} conflicts with the active trust store"
                    )));
                }
            } else {
                trust_store
                    .insert_base64(key_id.clone(), public_key)
                    .map_err(CatalogRegistryError::catalog_trust)?;
            }
            state.trust_keys.insert(key_id.clone(), public_key.clone());
        }
        if trust_store.is_empty() && !self.allow_empty_sources {
            return Err(CatalogRegistryError::configuration(
                "catalog trust store must contain at least one explicitly configured Ed25519 key",
            ));
        }
        for source in &self.bootstrap_sources {
            if let Some(existing) = state.sources.iter_mut().find(|value| value.id == source.id) {
                *existing = source.clone();
            } else {
                state.sources.push(source.clone());
            }
        }
        normalize_sources(&mut state.sources)?;
        if !self.allow_empty_sources && !state.sources.iter().any(|source| source.enabled) {
            return Err(CatalogRegistryError::configuration(
                "at least one trusted Catalog v2 source must be enabled",
            ));
        }
        let configured_source_ids = self
            .bootstrap_sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<BTreeSet<_>>();
        for source in state
            .sources
            .iter()
            .filter(|source| source.enabled || configured_source_ids.contains(source.id.as_str()))
        {
            self.load_verified_catalog_with_trust(source, &trust_store)?;
        }
        // This is the first persistence point. Trust keys and sources are
        // committed together only after every enabled source has verified.
        // Holding the trust write guard across the single durable commit also
        // prevents readers from observing the new source row before its key is
        // visible in memory. All network and file I/O above remains lock-free.
        let mut active_trust = self
            .trust_store
            .write()
            .map_err(|_| CatalogRegistryError::storage("catalog trust store lock is poisoned"))?;
        self.save_registry_state_unlocked(storage, &state)?;
        *active_trust = trust_store;
        Ok(())
    }

    pub(crate) fn source_page(
        &self,
        storage: &DurableStore,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<CatalogSourcePage, CatalogRegistryError> {
        let mut sources = self.load_sources_unlocked(storage)?;
        sources.sort_by(|left, right| left.id.cmp(&right.id));
        let start = match cursor {
            Some(cursor) => {
                let source_id = decode_source_cursor(cursor)?;
                sources.partition_point(|source| source.id <= source_id)
            }
            None => 0,
        };
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT);
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(CatalogRegistryError::new(
                400,
                "CATALOG_PAGE_LIMIT_INVALID",
                format!("limit must be between 1 and {MAX_PAGE_LIMIT}"),
            ));
        }
        let end = start.saturating_add(limit).min(sources.len());
        let items = sources[start..end].to_vec();
        let next_cursor = (end < sources.len())
            .then(|| items.last().map(|source| encode_source_cursor(&source.id)))
            .flatten();
        Ok(CatalogSourcePage { items, next_cursor })
    }

    pub(crate) fn register_source(
        &self,
        storage: &DurableStore,
        registration: CatalogSourceRegistration,
    ) -> Result<CatalogSource, CatalogRegistryError> {
        let (source, public_key) = registration.into_parts();
        let mut candidate_trust = self.trust_snapshot()?;
        self.apply_registration_key(
            &mut candidate_trust,
            &source.required_key_id,
            public_key.as_deref(),
        )?;
        self.validate_source_configuration_with_trust(&source, &candidate_trust)?;
        // Network and file I/O is intentionally completed before taking the
        // in-process mutation lock and before writing persistent state.
        self.load_verified_catalog_with_trust(&source, &candidate_trust)?;
        let _guard = self.source_mutation_lock.lock().map_err(|_| {
            CatalogRegistryError::storage("catalog source mutation lock is poisoned")
        })?;
        let mut state = self.load_registry_state_unlocked(storage)?;
        if state.sources.iter().any(|value| value.id == source.id) {
            return Err(CatalogRegistryError::new(
                409,
                "CATALOG_SOURCE_CONFLICT",
                format!("catalog source {} already exists", source.id),
            ));
        }
        let mut active_trust = self.trust_snapshot()?;
        self.merge_persisted_trust(&mut active_trust, &state.trust_keys)?;
        self.apply_registration_key(
            &mut active_trust,
            &source.required_key_id,
            public_key.as_deref(),
        )?;
        self.validate_source_configuration_with_trust(&source, &active_trust)?;
        if let Some(public_key) = public_key.as_deref() {
            if state
                .trust_keys
                .get(&source.required_key_id)
                .is_some_and(|existing| existing != public_key)
            {
                return Err(CatalogRegistryError::new(
                    409,
                    "CATALOG_TRUST_KEY_CONFLICT",
                    format!(
                        "trusted key ID {} is already bound to different key bytes",
                        source.required_key_id
                    ),
                ));
            }
            state
                .trust_keys
                .insert(source.required_key_id.clone(), public_key.to_string());
        }
        state.sources.push(source.clone());
        normalize_sources(&mut state.sources)?;
        let mut trust_guard = self
            .trust_store
            .write()
            .map_err(|_| CatalogRegistryError::storage("catalog trust store lock is poisoned"))?;
        self.save_registry_state_unlocked(storage, &state)?;
        *trust_guard = active_trust;
        Ok(source)
    }

    pub(crate) fn delete_source(
        &self,
        storage: &DurableStore,
        source_id: &str,
    ) -> Result<(), CatalogRegistryError> {
        let _guard = self.source_mutation_lock.lock().map_err(|_| {
            CatalogRegistryError::storage("catalog source mutation lock is poisoned")
        })?;
        let mut state = self.load_registry_state_unlocked(storage)?;
        let before = state.sources.len();
        state.sources.retain(|source| source.id != source_id);
        if state.sources.len() == before {
            return Err(CatalogRegistryError::new(
                404,
                "CATALOG_SOURCE_NOT_FOUND",
                format!("catalog source {source_id} was not found"),
            ));
        }
        if !self.allow_empty_sources && !state.sources.iter().any(|source| source.enabled) {
            return Err(CatalogRegistryError::new(
                409,
                "CATALOG_SOURCE_REQUIRED",
                "the last enabled trusted catalog source cannot be removed",
            ));
        }
        self.save_registry_state_unlocked(storage, &state)
    }

    pub(crate) fn packages(
        &self,
        storage: &DurableStore,
        query: &PackageQuery,
    ) -> Result<PackagePage, CatalogRegistryError> {
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let mut items = Vec::new();
        for source in self.enabled_sources(storage)? {
            let verified = self.load_verified_catalog(&source)?;
            for module in verified.catalog.modules {
                let searchable = format!(
                    "{}\n{}\n{}\n{}\n{}",
                    module.id,
                    module.name,
                    module.description,
                    module.kind,
                    module.tags.join("\n")
                )
                .to_ascii_lowercase();
                if search
                    .as_ref()
                    .is_some_and(|term| !searchable.contains(term))
                {
                    continue;
                }
                for release in module.releases {
                    if query
                        .channel
                        .is_some_and(|channel| release.channel != channel)
                    {
                        continue;
                    }
                    if query.platform.as_ref().is_some_and(|platform| {
                        !release
                            .platforms
                            .iter()
                            .any(|supported| supported.supports(platform))
                    }) {
                        continue;
                    }
                    items.push(CatalogPackageItem {
                        source_id: source.id.clone(),
                        catalog_id: verified.catalog_id.clone(),
                        module_id: module.id.clone(),
                        name: module.name.clone(),
                        description: module.description.clone(),
                        kind: module.kind.clone(),
                        tags: module.tags.clone(),
                        version: release.version,
                        channel: release.channel,
                        platforms: release.platforms,
                        min_orchestrator_version: release.min_orchestrator_version,
                        runtime_capabilities: release.runtime_capabilities,
                        metadata_sha256: release.metadata.sha256.to_string(),
                        oci_image: release.oci_image.to_string(),
                    });
                }
            }
        }
        items.sort_by_key(package_key);
        let start = match query.cursor.as_deref() {
            Some(cursor) => {
                let key = decode_cursor(cursor)?;
                items.partition_point(|item| package_key(item) <= key)
            }
            None => 0,
        };
        let limit = query.limit.unwrap_or(DEFAULT_PAGE_LIMIT);
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(CatalogRegistryError::new(
                400,
                "CATALOG_PAGE_LIMIT_INVALID",
                format!("limit must be between 1 and {MAX_PAGE_LIMIT}"),
            ));
        }
        let end = start.saturating_add(limit).min(items.len());
        let page_items = items[start..end].to_vec();
        let next_cursor = (end < items.len())
            .then(|| page_items.last().map(package_key))
            .flatten()
            .map(|key| encode_cursor(&key));
        Ok(PackagePage {
            items: page_items,
            next_cursor,
        })
    }

    pub(crate) fn resolve_install_plan(
        &self,
        storage: &DurableStore,
        source_id: Option<&str>,
        module_id: &str,
        version: Option<&str>,
        channel: ReleaseChannel,
        target_platform: TargetPlatform,
    ) -> Result<ResolvedCatalogPlan, CatalogRegistryError> {
        let requested_version = version
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(Version::parse)
            .transpose()
            .map_err(|error| {
                CatalogRegistryError::new(
                    422,
                    "CATALOG_VERSION_INVALID",
                    format!("requested version is not semver: {error}"),
                )
            })?;
        let mut candidates = Vec::new();
        for source in self.enabled_sources(storage)? {
            if source_id.is_some_and(|requested| requested != source.id) {
                continue;
            }
            let verified = self.load_verified_catalog(&source)?;
            if verified.catalog.module(module_id).is_some() {
                candidates.push((source, verified));
            }
        }
        if candidates.is_empty() {
            return Err(CatalogRegistryError::new(
                404,
                "CATALOG_MODULE_NOT_FOUND",
                source_id.map_or_else(
                    || format!("module {module_id} was not found in an enabled trusted catalog"),
                    |source| format!("module {module_id} was not found in catalog source {source}"),
                ),
            ));
        }
        if candidates.len() > 1 {
            return Err(CatalogRegistryError::new(
                409,
                "CATALOG_MODULE_AMBIGUOUS",
                format!(
                    "module {module_id} exists in multiple sources ({}); specify catalog_source_id",
                    candidates
                        .iter()
                        .map(|(source, _)| source.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        let (source, verified) = candidates.pop().expect("one candidate remains");
        let plan = verified
            .catalog
            .resolve_install_plan(&CatalogResolveRequest {
                module_id: module_id.to_string(),
                version: requested_version,
                channel,
                target_platform,
                orchestrator_version: self.orchestrator_version.clone(),
            })
            .map_err(CatalogRegistryError::catalog_resolution)?;
        Ok(ResolvedCatalogPlan {
            source_id: source.id,
            catalog_id: verified.catalog_id,
            verified_key_ids: verified.verified_key_ids,
            plan,
        })
    }

    /// Fetches and validates every metadata package before any release is
    /// published. A later registration error may leave earlier releases as
    /// explicitly imported metadata, but no runtime side effect has happened.
    pub(crate) fn fetch_release_documents(
        &self,
        storage: &DurableStore,
        resolved: &ResolvedCatalogPlan,
    ) -> Result<Vec<VerifiedReleaseDocument>, CatalogRegistryError> {
        let source = self
            .enabled_sources(storage)?
            .into_iter()
            .find(|source| source.id == resolved.source_id)
            .ok_or_else(|| {
                CatalogRegistryError::new(
                    409,
                    "CATALOG_SOURCE_CHANGED",
                    format!(
                        "catalog source {} is no longer enabled; resolve the install again",
                        resolved.source_id
                    ),
                )
            })?;
        // Re-verify the catalog and prove the plan still identifies exact entries.
        let latest = self.load_verified_catalog(&source)?;
        if latest.catalog_id != resolved.catalog_id
            || latest.verified_key_ids != resolved.verified_key_ids
        {
            return Err(CatalogRegistryError::new(
                409,
                "CATALOG_CHANGED",
                "catalog identity or verified signer set changed; resolve the install again",
            ));
        }
        let mut documents = Vec::with_capacity(resolved.plan.releases.len());
        for selection in &resolved.plan.releases {
            let catalog_release = latest
                .catalog
                .module(&selection.module_id)
                .and_then(|module| {
                    module.releases.iter().find(|release| {
                        release.version == selection.release.version
                            && release.channel == selection.release.channel
                    })
                })
                .ok_or_else(|| {
                    CatalogRegistryError::new(
                        409,
                        "CATALOG_CHANGED",
                        format!(
                            "resolved release {}@{} no longer exists",
                            selection.module_id, selection.release.version
                        ),
                    )
                })?;
            if catalog_release != &selection.release {
                return Err(CatalogRegistryError::new(
                    409,
                    "CATALOG_CHANGED",
                    format!(
                        "resolved release {}@{} changed; resolve the install again",
                        selection.module_id, selection.release.version
                    ),
                ));
            }
            let metadata_url =
                self.resolve_artifact_url(&source.url, &selection.release.metadata.url)?;
            let bytes =
                self.fetch_bytes(&metadata_url, &source.auth_secret_ref, MAX_METADATA_BYTES)?;
            let checksum = format!("sha256:{:x}", Sha256::digest(&bytes));
            if checksum != selection.release.metadata.sha256.as_str() {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_METADATA_CHECKSUM_MISMATCH",
                    format!(
                        "metadata for {}@{} expected {}, got {checksum}",
                        selection.module_id,
                        selection.release.version,
                        selection.release.metadata.sha256
                    ),
                ));
            }
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                CatalogRegistryError::new(
                    422,
                    "CATALOG_METADATA_INVALID",
                    format!("metadata {metadata_url} is not UTF-8 YAML or JSON: {error}"),
                )
            })?;
            let imported = external_release_import_from_yaml(text, &metadata_url, &checksum)
                .map_err(|error| {
                    CatalogRegistryError::new(
                        422,
                        "CATALOG_METADATA_INVALID",
                        format!("metadata {metadata_url} is invalid: {error}"),
                    )
                })?;
            if imported.release.service_name != selection.module_id
                || imported.release.version != selection.release.version.to_string()
            {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_METADATA_IDENTITY_MISMATCH",
                    format!(
                        "catalog selects {}@{} but metadata declares {}@{}",
                        selection.module_id,
                        selection.release.version,
                        imported.release.service_name,
                        imported.release.version
                    ),
                ));
            }
            if !imported.release.runtime.kind.eq_ignore_ascii_case("image")
                || imported.release.runtime.image != selection.release.oci_image.as_str()
            {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_METADATA_OCI_MISMATCH",
                    format!(
                        "metadata for {}@{} must declare catalog OCI image {}",
                        selection.module_id, selection.release.version, selection.release.oci_image
                    ),
                ));
            }
            let metadata_runtime_capabilities = release_supports_link_probe_v1(&imported.release)
                .then_some(RuntimeCapabilityV2::LinkProbeV1)
                .into_iter()
                .collect::<Vec<_>>();
            if selection.release.runtime_capabilities != metadata_runtime_capabilities {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_METADATA_CAPABILITY_MISMATCH",
                    format!(
                        "catalog and metadata runtime capability sets differ for {}@{}",
                        selection.module_id, selection.release.version
                    ),
                ));
            }
            let catalog_dependencies = selection
                .release
                .dependencies
                .iter()
                .map(|dependency| dependency.module_id.as_str())
                .collect::<BTreeSet<_>>();
            let metadata_dependencies = imported
                .release
                .dependencies
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if catalog_dependencies != metadata_dependencies {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_METADATA_DEPENDENCY_MISMATCH",
                    format!(
                        "catalog and metadata dependency sets differ for {}@{}",
                        selection.module_id, selection.release.version
                    ),
                ));
            }
            let offline_oci_layout =
                self.verify_offline_oci_layout(&source, &selection.release.oci_image)?;
            documents.push(VerifiedReleaseDocument {
                selection: selection.clone(),
                source_url: metadata_url,
                checksum,
                bytes,
                offline_oci_layout,
            });
        }
        Ok(documents)
    }

    fn enabled_sources(
        &self,
        storage: &DurableStore,
    ) -> Result<Vec<CatalogSource>, CatalogRegistryError> {
        let sources = self
            .load_sources_unlocked(storage)?
            .into_iter()
            .filter(|source| source.enabled)
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return Err(CatalogRegistryError::new(
                503,
                "CATALOG_SOURCE_UNAVAILABLE",
                "no enabled trusted catalog source is configured",
            ));
        }
        Ok(sources)
    }

    fn load_sources_unlocked(
        &self,
        storage: &DurableStore,
    ) -> Result<Vec<CatalogSource>, CatalogRegistryError> {
        self.load_registry_state_unlocked(storage)
            .map(|state| state.sources)
    }

    pub(crate) fn has_sources(&self, storage: &DurableStore) -> Result<bool, CatalogRegistryError> {
        self.load_sources_unlocked(storage)
            .map(|sources| !sources.is_empty())
    }

    pub(crate) fn has_enabled_sources(
        &self,
        storage: &DurableStore,
    ) -> Result<bool, CatalogRegistryError> {
        self.load_sources_unlocked(storage)
            .map(|sources| sources.into_iter().any(|source| source.enabled))
    }

    fn load_registry_state_unlocked(
        &self,
        storage: &DurableStore,
    ) -> Result<PersistedCatalogRegistry, CatalogRegistryError> {
        if let Some(state) = storage
            .get_state(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY)
            .map_err(CatalogRegistryError::durable)?
        {
            return Ok(state);
        }
        // Pre-v1 stored these independently. They remain read-only migration
        // inputs; bootstrap validates their complete combination before the
        // first atomic snapshot write.
        let trust_keys = storage
            .get_state(REGISTRY_NAMESPACE, TRUST_KEYS_KEY)
            .map_err(CatalogRegistryError::durable)?
            .unwrap_or_default();
        let sources = storage
            .get_state(REGISTRY_NAMESPACE, SOURCES_KEY)
            .map_err(CatalogRegistryError::durable)?
            .unwrap_or_default();
        Ok(PersistedCatalogRegistry {
            trust_keys,
            sources,
        })
    }

    fn save_registry_state_unlocked(
        &self,
        storage: &DurableStore,
        state: &PersistedCatalogRegistry,
    ) -> Result<(), CatalogRegistryError> {
        storage
            .put_state(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY, state)
            .map_err(CatalogRegistryError::durable)
    }

    fn merge_persisted_trust(
        &self,
        trust_store: &mut CatalogTrustStore,
        persisted: &BTreeMap<String, String>,
    ) -> Result<(), CatalogRegistryError> {
        for (key_id, public_key) in persisted {
            if trust_store.contains_key(key_id) {
                if !trust_store
                    .matches_base64(key_id, public_key)
                    .map_err(CatalogRegistryError::catalog_trust)?
                {
                    return Err(CatalogRegistryError::configuration(format!(
                        "durable Catalog trust key {key_id} conflicts with the explicitly configured key"
                    )));
                }
                continue;
            }
            if !self.allow_dynamic_trust {
                return Err(CatalogRegistryError::configuration(format!(
                    "durable Catalog trust key {key_id} is absent from the current explicit trust configuration"
                )));
            }
            trust_store
                .insert_base64(key_id.clone(), public_key)
                .map_err(CatalogRegistryError::catalog_trust)?;
        }
        Ok(())
    }

    fn validate_source_configuration(
        &self,
        source: &CatalogSource,
    ) -> Result<(), CatalogRegistryError> {
        let trust_store = self.trust_snapshot()?;
        self.validate_source_configuration_with_trust(source, &trust_store)
    }

    fn validate_source_configuration_with_trust(
        &self,
        source: &CatalogSource,
        trust_store: &CatalogTrustStore,
    ) -> Result<(), CatalogRegistryError> {
        validate_identifier("catalog source id", &source.id)?;
        if !trust_store.contains_key(&source.required_key_id) {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_TRUST_KEY_UNKNOWN",
                format!(
                    "catalog source {} requires unconfigured key {}",
                    source.id, source.required_key_id
                ),
            ));
        }
        self.validate_artifact_url(&source.url)?;
        validate_auth_secret_ref(&source.auth_secret_ref)?;
        for image in source.offline_oci_layouts.keys() {
            let image = image
                .parse::<CatalogOciImageReference>()
                .map_err(CatalogRegistryError::catalog_content)?;
            self.verify_offline_oci_layout(source, &image)?;
        }
        Ok(())
    }

    fn load_verified_catalog(
        &self,
        source: &CatalogSource,
    ) -> Result<VerifiedCatalog, CatalogRegistryError> {
        let trust_store = self.trust_snapshot()?;
        self.load_verified_catalog_with_trust(source, &trust_store)
    }

    fn load_verified_catalog_with_trust(
        &self,
        source: &CatalogSource,
        trust_store: &CatalogTrustStore,
    ) -> Result<VerifiedCatalog, CatalogRegistryError> {
        self.validate_source_configuration_with_trust(source, trust_store)?;
        let bytes = self.fetch_bytes(&source.url, &source.auth_secret_ref, MAX_CATALOG_BYTES)?;
        let catalog: CatalogV2 = serde_json::from_slice(&bytes).map_err(|error| {
            CatalogRegistryError::new(
                422,
                "CATALOG_DOCUMENT_INVALID",
                format!(
                    "catalog source {} is not Catalog v2 JSON: {error}",
                    source.id
                ),
            )
        })?;
        let signatures = catalog
            .validate_trusted(trust_store)
            .map_err(CatalogRegistryError::catalog_trust)?;
        if !signatures.contains(&source.required_key_id) {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_REQUIRED_SIGNATURE_MISSING",
                format!(
                    "catalog source {} was not signed by required key {}",
                    source.id, source.required_key_id
                ),
            ));
        }
        Ok(VerifiedCatalog {
            catalog_id: catalog.id.clone(),
            verified_key_ids: signatures.key_ids().to_vec(),
            catalog,
        })
    }

    fn trust_snapshot(&self) -> Result<CatalogTrustStore, CatalogRegistryError> {
        self.trust_store
            .read()
            .map(|store| (*store).clone())
            .map_err(|_| CatalogRegistryError::storage("catalog trust store lock is poisoned"))
    }

    fn apply_registration_key(
        &self,
        trust_store: &mut CatalogTrustStore,
        key_id: &str,
        public_key: Option<&str>,
    ) -> Result<(), CatalogRegistryError> {
        if trust_store.contains_key(key_id) {
            if let Some(public_key) = public_key
                && !trust_store
                    .matches_base64(key_id, public_key)
                    .map_err(CatalogRegistryError::catalog_trust)?
            {
                return Err(CatalogRegistryError::new(
                    409,
                    "CATALOG_TRUST_KEY_CONFLICT",
                    format!("trusted key ID {key_id} is already bound to different key bytes"),
                ));
            }
            return Ok(());
        }
        if !self.allow_dynamic_trust {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_TRUST_KEY_UNKNOWN",
                format!("catalog source requires unconfigured key {key_id}"),
            ));
        }
        let public_key = public_key.ok_or_else(|| {
            CatalogRegistryError::new(
                422,
                "CATALOG_TRUST_KEY_REQUIRED",
                format!(
                    "first use of trusted key ID {key_id} requires public_key in the registration request"
                ),
            )
        })?;
        trust_store
            .insert_base64(key_id.to_string(), public_key)
            .map_err(CatalogRegistryError::catalog_trust)
    }

    fn fetch_bytes(
        &self,
        url: &str,
        auth_secret_ref: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CatalogRegistryError> {
        if is_https(url) {
            return self.fetch_https(url, auth_secret_ref, max_bytes);
        }
        if url.trim_start().to_ascii_lowercase().starts_with("http://") {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_HTTPS_REQUIRED",
                "remote catalog and metadata URLs must use HTTPS",
            ));
        }
        let path = self.resolve_local_path(url, None)?;
        let metadata = fs::metadata(&path).map_err(|error| {
            CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_CONTENT_MISSING",
                format!("read local catalog content {}: {error}", path.display()),
            )
        })?;
        if !metadata.is_file() || metadata.len() > max_bytes as u64 {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_CONTENT_SIZE_INVALID",
                format!(
                    "local catalog content {} must be a file no larger than {max_bytes} bytes",
                    path.display()
                ),
            ));
        }
        fs::read(&path).map_err(|error| {
            CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_CONTENT_MISSING",
                format!("read local catalog content {}: {error}", path.display()),
            )
        })
    }

    fn fetch_https(
        &self,
        url: &str,
        auth_secret_ref: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CatalogRegistryError> {
        let token = github_token(url, auth_secret_ref)?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        let mut current = url.trim().to_string();
        let mut hops = 0_u8;
        loop {
            if !is_https(&current) {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_HTTPS_REQUIRED",
                    "remote redirect targets must use HTTPS",
                ));
            }
            orchestrator_legacy::validate_outbound_url(&current).map_err(|error| {
                CatalogRegistryError::new(
                    422,
                    "CATALOG_URL_BLOCKED",
                    format!("fetch {url} was blocked: {error}"),
                )
            })?;
            let mut request = agent.get(&current).header("User-Agent", USER_AGENT);
            if hops == 0
                && let Some(token) = token.as_deref()
            {
                request = request.header("Authorization", &format!("Bearer {token}"));
                request = request.header("Accept", "application/octet-stream, application/json");
            }
            let response = request.call().map_err(|error| {
                CatalogRegistryError::new(
                    503,
                    "CATALOG_FETCH_FAILED",
                    format!("fetch {url} failed: {error}"),
                )
            })?;
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if hops >= MAX_REDIRECTS {
                    return Err(CatalogRegistryError::new(
                        503,
                        "CATALOG_FETCH_FAILED",
                        format!("fetch {url} exceeded {MAX_REDIRECTS} redirects"),
                    ));
                }
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        CatalogRegistryError::new(
                            503,
                            "CATALOG_FETCH_FAILED",
                            format!("fetch {url} returned a redirect without Location"),
                        )
                    })?;
                current = resolve_outbound_redirect(&current, location).map_err(|error| {
                    CatalogRegistryError::new(
                        422,
                        "CATALOG_URL_BLOCKED",
                        format!("fetch {url} redirect was rejected: {error}"),
                    )
                })?;
                hops += 1;
                continue;
            }
            let mut bytes = Vec::new();
            response
                .into_body()
                .into_reader()
                .take(max_bytes as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    CatalogRegistryError::new(
                        503,
                        "CATALOG_FETCH_FAILED",
                        format!("read {url} failed: {error}"),
                    )
                })?;
            if !(200..=299).contains(&status) {
                return Err(CatalogRegistryError::new(
                    503,
                    "CATALOG_FETCH_FAILED",
                    format!("fetch {url} returned HTTP {status}"),
                ));
            }
            if bytes.len() > max_bytes {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_CONTENT_SIZE_INVALID",
                    format!("content from {url} exceeds {max_bytes} bytes"),
                ));
            }
            return Ok(bytes);
        }
    }

    fn validate_artifact_url(&self, url: &str) -> Result<(), CatalogRegistryError> {
        if is_https(url) {
            return Ok(());
        }
        if url.trim_start().to_ascii_lowercase().starts_with("http://") {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_HTTPS_REQUIRED",
                "remote catalog and metadata URLs must use HTTPS",
            ));
        }
        self.resolve_local_path(url, None).map(|_| ())
    }

    fn resolve_artifact_url(
        &self,
        catalog_url: &str,
        artifact_url: &str,
    ) -> Result<String, CatalogRegistryError> {
        let artifact = artifact_url.trim();
        if artifact.is_empty() {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_ARTIFACT_URL_INVALID",
                "catalog metadata URL is empty",
            ));
        }
        if is_https(artifact) {
            return Ok(artifact.to_string());
        }
        if artifact.to_ascii_lowercase().starts_with("http://") {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_HTTPS_REQUIRED",
                "remote metadata URLs must use HTTPS",
            ));
        }
        if is_https(catalog_url) {
            let resolved = resolve_outbound_redirect(catalog_url, artifact).map_err(|error| {
                CatalogRegistryError::new(
                    422,
                    "CATALOG_ARTIFACT_URL_INVALID",
                    format!("resolve metadata URL {artifact}: {error}"),
                )
            })?;
            if !is_https(&resolved) {
                return Err(CatalogRegistryError::new(
                    422,
                    "CATALOG_HTTPS_REQUIRED",
                    "remote metadata URLs must use HTTPS",
                ));
            }
            return Ok(resolved);
        }
        let catalog_path = self.resolve_local_path(catalog_url, None)?;
        let parent = catalog_path.parent().ok_or_else(|| {
            CatalogRegistryError::new(
                422,
                "CATALOG_ARTIFACT_URL_INVALID",
                "local catalog path has no parent directory",
            )
        })?;
        let path = self.resolve_local_path(artifact, Some(parent))?;
        path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
            CatalogRegistryError::new(
                422,
                "CATALOG_ARTIFACT_URL_INVALID",
                "local artifact path must be valid UTF-8",
            )
        })
    }

    fn resolve_local_path(
        &self,
        value: &str,
        relative_to: Option<&Path>,
    ) -> Result<PathBuf, CatalogRegistryError> {
        let raw = value
            .trim()
            .strip_prefix("local://")
            .or_else(|| value.trim().strip_prefix("file://"))
            .unwrap_or(value.trim());
        if raw.is_empty() || raw.chars().any(char::is_control) {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_PATH_INVALID",
                "local catalog path is empty or contains control characters",
            ));
        }
        let requested = Path::new(raw);
        if requested
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_PATH_INVALID",
                "local catalog paths must be repository-relative and cannot contain '..'",
            ));
        }
        let base = relative_to.unwrap_or(&self.repo_root);
        let path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            base.join(requested)
        };
        let normalized_root = canonical_or_absolute(&self.repo_root)?;
        let normalized_path = canonical_or_absolute(&path)?;
        if !normalized_path.starts_with(&normalized_root) {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_PATH_INVALID",
                "local catalog path escapes the configured repository root",
            ));
        }
        Ok(path)
    }

    fn verify_offline_oci_layout(
        &self,
        source: &CatalogSource,
        image: &CatalogOciImageReference,
    ) -> Result<Option<PathBuf>, CatalogRegistryError> {
        let Some(relative) = source.offline_oci_layouts.get(image.as_str()) else {
            return Ok(None);
        };
        let root = self.resolve_local_path(relative, None)?;
        let layout = read_limited_file(&root.join("oci-layout"), 1024)?;
        let layout: Value = serde_json::from_slice(&layout).map_err(|error| {
            CatalogRegistryError::new(
                422,
                "CATALOG_OFFLINE_OCI_INVALID",
                format!("parse {}/oci-layout: {error}", root.display()),
            )
        })?;
        if layout.get("imageLayoutVersion").and_then(Value::as_str) != Some("1.0.0") {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_OFFLINE_OCI_INVALID",
                format!(
                    "{} is not an OCI image-layout v1.0.0 directory",
                    root.display()
                ),
            ));
        }
        let index = read_limited_file(&root.join("index.json"), MAX_CATALOG_BYTES)?;
        let index: Value = serde_json::from_slice(&index).map_err(|error| {
            CatalogRegistryError::new(
                422,
                "CATALOG_OFFLINE_OCI_INVALID",
                format!("parse {}/index.json: {error}", root.display()),
            )
        })?;
        let digest = image.digest().as_str();
        let present = index
            .get("manifests")
            .and_then(Value::as_array)
            .is_some_and(|manifests| {
                manifests
                    .iter()
                    .any(|manifest| manifest.get("digest").and_then(Value::as_str) == Some(digest))
            });
        if !present {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_OFFLINE_OCI_DIGEST_MISSING",
                format!("OCI layout {} does not contain {digest}", root.display()),
            ));
        }
        let blob = root.join("blobs").join("sha256").join(image.digest().hex());
        if !blob.is_file() {
            return Err(CatalogRegistryError::new(
                422,
                "CATALOG_OFFLINE_OCI_DIGEST_MISSING",
                format!(
                    "OCI layout {} is missing manifest blob {digest}",
                    root.display()
                ),
            ));
        }
        Ok(Some(root))
    }
}

#[derive(Debug)]
struct VerifiedCatalog {
    catalog_id: String,
    verified_key_ids: Vec<String>,
    catalog: CatalogV2,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{detail}")]
pub(crate) struct CatalogRegistryError {
    status: u16,
    code: &'static str,
    detail: String,
}

impl CatalogRegistryError {
    fn new(status: u16, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
        }
    }

    pub(crate) fn status(&self) -> u16 {
        self.status
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    fn configuration(detail: impl Into<String>) -> Self {
        Self::new(500, "CATALOG_CONFIGURATION_INVALID", detail)
    }

    fn storage(detail: impl Into<String>) -> Self {
        Self::new(500, "CATALOG_STORAGE_ERROR", detail)
    }

    fn durable(error: DurableError) -> Self {
        Self::storage(error.to_string())
    }

    fn catalog_trust(error: CatalogV2Error) -> Self {
        Self::new(422, "CATALOG_TRUST_REJECTED", error.to_string())
    }

    fn catalog_resolution(error: CatalogV2Error) -> Self {
        Self::new(422, "CATALOG_RESOLUTION_REJECTED", error.to_string())
    }

    fn catalog_content(error: CatalogV2Error) -> Self {
        Self::new(422, "CATALOG_CONTENT_INVALID", error.to_string())
    }
}

fn normalize_sources(sources: &mut [CatalogSource]) -> Result<(), CatalogRegistryError> {
    sources.sort_by(|left, right| left.id.cmp(&right.id));
    if let Some(duplicate) = sources
        .windows(2)
        .find(|pair| pair[0].id == pair[1].id)
        .map(|pair| pair[0].id.clone())
    {
        return Err(CatalogRegistryError::new(
            409,
            "CATALOG_SOURCE_CONFLICT",
            format!("duplicate catalog source {duplicate}"),
        ));
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<(), CatalogRegistryError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        })
    {
        return Err(CatalogRegistryError::new(
            422,
            "CATALOG_SOURCE_INVALID",
            format!("{field} contains invalid characters"),
        ));
    }
    Ok(())
}

fn validate_auth_secret_ref(value: &str) -> Result<(), CatalogRegistryError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    let Some(name) = value.strip_prefix("env:") else {
        return Err(CatalogRegistryError::new(
            422,
            "CATALOG_AUTH_SECRET_REF_INVALID",
            "auth_secret_ref must use env:VARIABLE_NAME",
        ));
    };
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(CatalogRegistryError::new(
            422,
            "CATALOG_AUTH_SECRET_REF_INVALID",
            "auth_secret_ref environment variable must contain only A-Z, 0-9, and underscore",
        ));
    }
    Ok(())
}

fn github_token(url: &str, auth_secret_ref: &str) -> Result<Option<String>, CatalogRegistryError> {
    let explicit = auth_secret_ref.strip_prefix("env:");
    if explicit.is_some() && !is_github_url(url) {
        return Err(CatalogRegistryError::new(
            422,
            "CATALOG_AUTH_HOST_INVALID",
            "catalog authentication tokens are supported only for GitHub HTTPS hosts",
        ));
    }
    let token = match explicit {
        Some(name) => Some(std::env::var(name).map_err(|_| {
            CatalogRegistryError::new(
                503,
                "CATALOG_AUTH_SECRET_MISSING",
                format!("catalog auth environment variable {name} is not configured"),
            )
        })?),
        None if is_github_url(url) => std::env::var(FALLBACK_GITHUB_TOKEN_ENV)
            .ok()
            .or_else(|| std::env::var(DEFAULT_GITHUB_TOKEN_ENV).ok()),
        None => None,
    };
    Ok(token.filter(|value| !value.trim().is_empty()))
}

fn is_https(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("https://")
}

fn is_github_url(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("https://") else {
        return false;
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "github.com"
        || host == "api.github.com"
        || host == "raw.githubusercontent.com"
        || host == "objects.githubusercontent.com"
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf, CatalogRegistryError> {
    if path.exists() {
        return path.canonicalize().map_err(|error| {
            CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_PATH_INVALID",
                format!("resolve local path {}: {error}", path.display()),
            )
        });
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| CatalogRegistryError::storage(error.to_string()))?
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_PATH_INVALID",
                format!("local path {} has no existing ancestor", path.display()),
            )
        })?;
        suffix.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            CatalogRegistryError::new(
                422,
                "CATALOG_LOCAL_PATH_INVALID",
                format!("local path {} has no existing ancestor", path.display()),
            )
        })?;
    }
    let mut normalized = ancestor.canonicalize().map_err(|error| {
        CatalogRegistryError::new(
            422,
            "CATALOG_LOCAL_PATH_INVALID",
            format!("resolve local path {}: {error}", path.display()),
        )
    })?;
    for component in suffix.into_iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

fn read_limited_file(path: &Path, max_bytes: usize) -> Result<Vec<u8>, CatalogRegistryError> {
    let metadata = fs::metadata(path).map_err(|error| {
        CatalogRegistryError::new(
            422,
            "CATALOG_OFFLINE_OCI_INVALID",
            format!("read {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_file() || metadata.len() > max_bytes as u64 {
        return Err(CatalogRegistryError::new(
            422,
            "CATALOG_OFFLINE_OCI_INVALID",
            format!("{} is not a bounded regular file", path.display()),
        ));
    }
    fs::read(path).map_err(|error| {
        CatalogRegistryError::new(
            422,
            "CATALOG_OFFLINE_OCI_INVALID",
            format!("read {}: {error}", path.display()),
        )
    })
}

fn package_key(item: &CatalogPackageItem) -> String {
    format!(
        "{}\0{}\0{}\0{}\0{}",
        item.source_id, item.catalog_id, item.module_id, item.version, item.channel
    )
}

fn encode_cursor(value: &str) -> String {
    encode_hex_cursor("c1-", value)
}

fn decode_cursor(cursor: &str) -> Result<String, CatalogRegistryError> {
    decode_hex_cursor("c1-", cursor)
}

fn encode_source_cursor(source_id: &str) -> String {
    encode_hex_cursor("s1-", source_id)
}

fn decode_source_cursor(cursor: &str) -> Result<String, CatalogRegistryError> {
    decode_hex_cursor("s1-", cursor)
}

fn encode_hex_cursor(prefix: &str, value: &str) -> String {
    let mut cursor = String::with_capacity(prefix.len() + value.len() * 2);
    cursor.push_str(prefix);
    for byte in value.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(cursor, "{byte:02x}");
    }
    cursor
}

fn decode_hex_cursor(prefix: &str, cursor: &str) -> Result<String, CatalogRegistryError> {
    let Some(hex) = cursor.strip_prefix(prefix) else {
        return Err(CatalogRegistryError::new(
            400,
            "CATALOG_CURSOR_INVALID",
            "cursor has the wrong Catalog v2 resource kind",
        ));
    };
    decode_hex_cursor_bytes(hex)
}

fn decode_hex_cursor_bytes(hex: &str) -> Result<String, CatalogRegistryError> {
    if hex.is_empty()
        || !hex.len().is_multiple_of(2)
        || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CatalogRegistryError::new(
            400,
            "CATALOG_CURSOR_INVALID",
            "cursor has invalid encoding",
        ));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let text = std::str::from_utf8(pair).expect("hex is ASCII");
        bytes.push(u8::from_str_radix(text, 16).expect("hex was validated"));
    }
    String::from_utf8(bytes).map_err(|_| {
        CatalogRegistryError::new(
            400,
            "CATALOG_CURSOR_INVALID",
            "cursor does not contain UTF-8",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    use orchestrator_manager::catalog_v2::Ed25519Signature;
    use orchestrator_storage::SqliteOrchestratorStore;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn source() -> CatalogSource {
        CatalogSource {
            id: "fixture-source".to_string(),
            url: "catalog.json".to_string(),
            required_key_id: "fixture-key".to_string(),
            auth_secret_ref: String::new(),
            enabled: true,
            offline_oci_layouts: BTreeMap::new(),
        }
    }

    fn trust(signing_key: &SigningKey) -> CatalogTrustStore {
        let mut trust = CatalogTrustStore::new();
        trust
            .insert("fixture-key", signing_key.verifying_key().to_bytes())
            .expect("trust key");
        trust
    }

    fn catalog(signing_key: &SigningKey) -> CatalogV2 {
        let release = |metadata_url: &str| {
            serde_json::json!({
                "version": "1.0.0",
                "channel": "stable",
                "platforms": [{"os": "linux", "arch": "x86_64"}],
                "min_orchestrator_version": "0.1.0",
                "dependencies": [],
                "metadata": {
                    "url": metadata_url,
                    "sha256": format!("sha256:{}", "a".repeat(64)),
                },
                "oci_image": format!("registry.example/fixture@{DIGEST}"),
            })
        };
        let mut catalog: CatalogV2 = serde_json::from_value(serde_json::json!({
            "schema_version": 2,
            "id": "fixture-catalog",
            "name": "Fixture Catalog",
            "modules": [
                {
                    "id": "alpha-api",
                    "name": "Alpha API",
                    "description": "first searchable API",
                    "kind": "backend-api",
                    "tags": ["api"],
                    "releases": [release("alpha.yaml")],
                },
                {
                    "id": "beta-api",
                    "name": "Beta API",
                    "description": "second searchable API",
                    "kind": "backend-api",
                    "tags": ["api"],
                    "releases": [release("beta.yaml")],
                }
            ],
            "signatures": [],
        }))
        .expect("catalog");
        let signature = signing_key.sign(&catalog.signing_payload_jcs().expect("payload"));
        catalog.signatures.push(Ed25519Signature {
            key_id: "fixture-key".to_string(),
            algorithm: "Ed25519".to_string(),
            signature: STANDARD.encode(signature.to_bytes()),
        });
        catalog
    }

    fn durable(root: &Path) -> DurableStore {
        DurableStore::Sqlite(
            SqliteOrchestratorStore::open(root.join("registry.db")).expect("sqlite"),
        )
    }

    #[test]
    fn blank_catalog_environment_values_are_unconfigured_as_a_pair() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(
            CatalogRegistry::from_env_values(
                root.path(),
                Some(" \t ".to_string()),
                Some("\r\n".to_string()),
            )
            .expect("blank pair")
            .is_none()
        );

        let missing_trust = CatalogRegistry::from_env_values(
            root.path(),
            Some("   ".to_string()),
            Some("[]".to_string()),
        )
        .expect_err("a nonblank source list still requires trust keys");
        assert_eq!(missing_trust.code(), "CATALOG_CONFIGURATION_INVALID");
        assert!(missing_trust.to_string().contains(TRUST_KEYS_ENV));

        let missing_sources = CatalogRegistry::from_env_values(
            root.path(),
            Some("{}".to_string()),
            Some(" \n ".to_string()),
        )
        .expect_err("nonblank trust keys still require a source list");
        assert_eq!(missing_sources.code(), "CATALOG_CONFIGURATION_INVALID");
        assert!(missing_sources.to_string().contains(SOURCES_ENV));
    }

    #[test]
    fn cursor_round_trip_is_stable() {
        let value = "source\0catalog\0module\u{0}1.2.3\0stable";
        assert_eq!(decode_cursor(&encode_cursor(value)).expect("decode"), value);
    }

    #[test]
    fn empty_trust_store_and_unknown_required_key_fail_closed() {
        let root = tempfile::tempdir().expect("tempdir");
        let empty = CatalogRegistry::new(root.path(), CatalogTrustStore::new(), vec![])
            .expect_err("empty trust must fail");
        assert_eq!(empty.code(), "CATALOG_CONFIGURATION_INVALID");

        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut unknown_source = source();
        unknown_source.required_key_id = "unknown-key".to_string();
        let unknown = CatalogRegistry::new(root.path(), trust(&signing_key), vec![unknown_source])
            .expect_err("unknown required key must fail");
        assert_eq!(unknown.code(), "CATALOG_TRUST_KEY_UNKNOWN");

        let mut missing_offline = source();
        missing_offline.offline_oci_layouts.insert(
            format!("registry.example/fixture@{DIGEST}"),
            "missing-oci-layout".to_string(),
        );
        let missing = CatalogRegistry::new(root.path(), trust(&signing_key), vec![missing_offline])
            .expect_err("missing offline OCI content must fail");
        assert_eq!(missing.code(), "CATALOG_OFFLINE_OCI_INVALID");
    }

    #[test]
    fn invalid_catalog_signature_is_never_persisted() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let public_key = STANDARD.encode(signing_key.verifying_key().to_bytes());
        let mut catalog = catalog(&signing_key);
        catalog.signatures[0].signature = STANDARD.encode([0_u8; 64]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&catalog).expect("serialize"),
        )
        .expect("write catalog");
        let registry = CatalogRegistry::new_internal(
            root.path(),
            trust(&signing_key),
            BTreeMap::from([("fixture-key".to_string(), public_key)]),
            vec![source()],
            false,
            false,
        )
        .expect("registry");
        let durable = durable(root.path());
        let error = registry
            .bootstrap(&durable)
            .expect_err("bad signature must fail");
        assert_eq!(error.code(), "CATALOG_TRUST_REJECTED");
        assert!(
            registry
                .source_page(&durable, None, None)
                .expect("sources")
                .items
                .is_empty()
        );
        assert!(
            durable
                .get_state::<PersistedCatalogRegistry>(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY)
                .expect("registry state")
                .is_none(),
            "the trust key and source must not be persisted before signature verification"
        );
        assert!(
            durable
                .get_state::<BTreeMap<String, String>>(REGISTRY_NAMESPACE, TRUST_KEYS_KEY)
                .expect("legacy trust state")
                .is_none()
        );
        assert!(
            durable
                .get_state::<Vec<CatalogSource>>(REGISTRY_NAMESPACE, SOURCES_KEY)
                .expect("legacy source state")
                .is_none()
        );
    }

    #[test]
    fn trusted_packages_support_filtering_and_stable_cursor_pagination() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&catalog(&signing_key)).expect("serialize"),
        )
        .expect("write catalog");
        let registry = CatalogRegistry::new(root.path(), trust(&signing_key), vec![source()])
            .expect("registry");
        let durable = durable(root.path());
        registry.bootstrap(&durable).expect("bootstrap");
        let first = registry
            .packages(
                &durable,
                &PackageQuery {
                    search: Some("api".to_string()),
                    channel: Some(ReleaseChannel::Stable),
                    platform: Some(TargetPlatform::new("linux", "amd64")),
                    cursor: None,
                    limit: Some(1),
                },
            )
            .expect("first page");
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].module_id, "alpha-api");
        let second = registry
            .packages(
                &durable,
                &PackageQuery {
                    search: Some("api".to_string()),
                    channel: Some(ReleaseChannel::Stable),
                    platform: Some(TargetPlatform::new("linux", "x86_64")),
                    cursor: first.next_cursor,
                    limit: Some(1),
                },
            )
            .expect("second page");
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].module_id, "beta-api");
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn desktop_can_bootstrap_first_trust_key_and_reload_it_after_restart() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&catalog(&signing_key)).expect("serialize"),
        )
        .expect("write catalog");
        let durable = durable(root.path());
        let registry = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        registry
            .bootstrap(&durable)
            .expect("empty Desktop bootstrap is valid");
        assert!(!registry.has_sources(&durable).expect("source state"));
        assert!(
            !registry
                .has_enabled_sources(&durable)
                .expect("enabled source state")
        );

        let missing_key = registry
            .register_source(
                &durable,
                CatalogSourceRegistration {
                    id: source().id,
                    url: source().url,
                    required_key_id: source().required_key_id,
                    auth_secret_ref: String::new(),
                    enabled: true,
                    offline_oci_layouts: BTreeMap::new(),
                    public_key: None,
                },
            )
            .expect_err("first key must be explicit");
        assert_eq!(missing_key.code(), "CATALOG_TRUST_KEY_REQUIRED");
        assert!(!registry.has_sources(&durable).expect("no partial source"));

        registry
            .register_source(
                &durable,
                CatalogSourceRegistration {
                    id: source().id,
                    url: source().url,
                    required_key_id: source().required_key_id,
                    auth_secret_ref: String::new(),
                    enabled: true,
                    offline_oci_layouts: BTreeMap::new(),
                    public_key: Some(STANDARD.encode(signing_key.verifying_key().to_bytes())),
                },
            )
            .expect("register first trusted source");
        assert!(
            registry
                .has_enabled_sources(&durable)
                .expect("source ready")
        );

        let restarted = CatalogRegistry::desktop(root.path()).expect("restarted registry");
        restarted
            .bootstrap(&durable)
            .expect("reload durable trust and source");
        let page = restarted
            .packages(
                &durable,
                &PackageQuery {
                    limit: Some(10),
                    ..PackageQuery::default()
                },
            )
            .expect("catalog remains usable after restart");
        assert_eq!(page.items.len(), 2);

        restarted
            .delete_source(&durable, "fixture-source")
            .expect("Desktop may remove its final source");
        assert!(!restarted.has_sources(&durable).expect("source removed"));
        CatalogRegistry::desktop(root.path())
            .expect("third registry")
            .bootstrap(&durable)
            .expect("empty source set remains bootable");
    }

    #[test]
    fn desktop_bad_first_signature_persists_neither_key_nor_source() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        let mut invalid_catalog = catalog(&signing_key);
        invalid_catalog.signatures[0].signature = STANDARD.encode([0_u8; 64]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&invalid_catalog).expect("serialize"),
        )
        .expect("write invalid catalog");
        let durable = durable(root.path());
        let registry = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        registry.bootstrap(&durable).expect("empty bootstrap");

        let error = registry
            .register_source(
                &durable,
                CatalogSourceRegistration {
                    id: source().id,
                    url: source().url,
                    required_key_id: source().required_key_id,
                    auth_secret_ref: String::new(),
                    enabled: true,
                    offline_oci_layouts: BTreeMap::new(),
                    public_key: Some(STANDARD.encode(signing_key.verifying_key().to_bytes())),
                },
            )
            .expect_err("invalid first signature");
        assert_eq!(error.code(), "CATALOG_TRUST_REJECTED");
        let state = durable
            .get_state::<PersistedCatalogRegistry>(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY)
            .expect("registry state")
            .expect("empty bootstrap snapshot");
        assert!(state.trust_keys.is_empty());
        assert!(state.sources.is_empty());

        let restarted = CatalogRegistry::desktop(root.path()).expect("restart registry");
        restarted.bootstrap(&durable).expect("restart bootstrap");
        let missing_key = restarted
            .register_source(
                &durable,
                CatalogSourceRegistration {
                    id: source().id,
                    url: source().url,
                    required_key_id: source().required_key_id,
                    auth_secret_ref: String::new(),
                    enabled: true,
                    offline_oci_layouts: BTreeMap::new(),
                    public_key: None,
                },
            )
            .expect_err("failed first key must not survive restart");
        assert_eq!(missing_key.code(), "CATALOG_TRUST_KEY_REQUIRED");
    }

    #[test]
    fn legacy_split_state_is_verified_before_atomic_migration() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&catalog(&signing_key)).expect("serialize"),
        )
        .expect("write catalog");
        let durable = durable(root.path());
        durable
            .put_state(
                REGISTRY_NAMESPACE,
                TRUST_KEYS_KEY,
                &BTreeMap::from([(
                    "fixture-key".to_string(),
                    STANDARD.encode(signing_key.verifying_key().to_bytes()),
                )]),
            )
            .expect("legacy trust state");
        durable
            .put_state(REGISTRY_NAMESPACE, SOURCES_KEY, &vec![source()])
            .expect("legacy source state");

        let registry = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        registry
            .bootstrap(&durable)
            .expect("verify and migrate legacy state");
        let migrated = durable
            .get_state::<PersistedCatalogRegistry>(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY)
            .expect("read migrated state")
            .expect("atomic snapshot");
        assert_eq!(migrated.sources, vec![source()]);
        assert_eq!(migrated.trust_keys.len(), 1);
        assert_eq!(
            registry
                .packages(
                    &durable,
                    &PackageQuery {
                        limit: Some(10),
                        ..PackageQuery::default()
                    },
                )
                .expect("migrated catalog")
                .items
                .len(),
            2
        );
    }

    #[test]
    fn invalid_legacy_split_state_is_not_migrated() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        let mut invalid_catalog = catalog(&signing_key);
        invalid_catalog.signatures[0].signature = STANDARD.encode([0_u8; 64]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&invalid_catalog).expect("serialize"),
        )
        .expect("write invalid catalog");
        let durable = durable(root.path());
        durable
            .put_state(
                REGISTRY_NAMESPACE,
                TRUST_KEYS_KEY,
                &BTreeMap::from([(
                    "fixture-key".to_string(),
                    STANDARD.encode(signing_key.verifying_key().to_bytes()),
                )]),
            )
            .expect("legacy trust state");
        durable
            .put_state(REGISTRY_NAMESPACE, SOURCES_KEY, &vec![source()])
            .expect("legacy source state");

        let registry = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        let error = registry
            .bootstrap(&durable)
            .expect_err("invalid legacy catalog must fail migration");
        assert_eq!(error.code(), "CATALOG_TRUST_REJECTED");
        assert!(
            durable
                .get_state::<PersistedCatalogRegistry>(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY)
                .expect("atomic state")
                .is_none(),
            "legacy split state must not be promoted before signature verification"
        );
    }

    #[test]
    fn production_registry_rejects_a_durable_dynamic_trust_key() {
        let root = tempfile::tempdir().expect("tempdir");
        let desktop_key = SigningKey::from_bytes(&[9_u8; 32]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&catalog(&desktop_key)).expect("serialize"),
        )
        .expect("write catalog");
        let durable = durable(root.path());
        let desktop = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        desktop.bootstrap(&durable).expect("desktop bootstrap");
        desktop
            .register_source(
                &durable,
                CatalogSourceRegistration {
                    id: source().id,
                    url: source().url,
                    required_key_id: source().required_key_id,
                    auth_secret_ref: String::new(),
                    enabled: true,
                    offline_oci_layouts: BTreeMap::new(),
                    public_key: Some(STANDARD.encode(desktop_key.verifying_key().to_bytes())),
                },
            )
            .expect("desktop dynamic trust");

        let production_key = SigningKey::from_bytes(&[10_u8; 32]);
        let mut production_trust = CatalogTrustStore::new();
        production_trust
            .insert("production-key", production_key.verifying_key().to_bytes())
            .expect("production trust");
        let production = CatalogRegistry::new(root.path(), production_trust, vec![])
            .expect("production registry configuration");
        let error = production
            .bootstrap(&durable)
            .expect_err("production must not absorb Desktop trust");
        assert_eq!(error.code(), "CATALOG_CONFIGURATION_INVALID");
        assert!(
            error
                .to_string()
                .contains("absent from the current explicit")
        );
    }

    #[test]
    fn concurrent_first_trust_registration_has_one_durable_winner() {
        use std::sync::{Arc, Barrier};

        let root = tempfile::tempdir().expect("tempdir");
        let first_key = SigningKey::from_bytes(&[9_u8; 32]);
        let second_key = SigningKey::from_bytes(&[10_u8; 32]);
        fs::write(
            root.path().join("catalog-first.json"),
            serde_json::to_vec(&catalog(&first_key)).expect("serialize first"),
        )
        .expect("write first catalog");
        fs::write(
            root.path().join("catalog-second.json"),
            serde_json::to_vec(&catalog(&second_key)).expect("serialize second"),
        )
        .expect("write second catalog");
        let durable = durable(root.path());
        let registry = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        registry.bootstrap(&durable).expect("desktop bootstrap");
        let barrier = Arc::new(Barrier::new(2));

        let handles = [
            ("first", "catalog-first.json", first_key),
            ("second", "catalog-second.json", second_key),
        ]
        .into_iter()
        .map(|(id, url, signing_key)| {
            let registry = registry.clone();
            let durable = durable.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                registry.register_source(
                    &durable,
                    CatalogSourceRegistration {
                        id: id.to_string(),
                        url: url.to_string(),
                        required_key_id: "fixture-key".to_string(),
                        auth_secret_ref: String::new(),
                        enabled: true,
                        offline_oci_layouts: BTreeMap::new(),
                        public_key: Some(STANDARD.encode(signing_key.verifying_key().to_bytes())),
                    },
                )
            })
        })
        .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().expect("registration thread"))
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        let loser = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .expect("one conflicting registration");
        assert_eq!(loser.code(), "CATALOG_TRUST_KEY_CONFLICT");

        let state = durable
            .get_state::<PersistedCatalogRegistry>(REGISTRY_NAMESPACE, REGISTRY_STATE_KEY)
            .expect("registry state")
            .expect("atomic state");
        assert_eq!(state.trust_keys.len(), 1);
        assert_eq!(state.sources.len(), 1);
        let restarted = CatalogRegistry::desktop(root.path()).expect("restart registry");
        restarted
            .bootstrap(&durable)
            .expect("winner remains valid after restart");
        assert_eq!(
            restarted
                .source_page(&durable, None, None)
                .expect("source page")
                .items
                .len(),
            1
        );
    }

    #[test]
    fn desktop_rejects_rebinding_a_trusted_key_id() {
        let root = tempfile::tempdir().expect("tempdir");
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        fs::write(
            root.path().join("catalog.json"),
            serde_json::to_vec(&catalog(&signing_key)).expect("serialize"),
        )
        .expect("write catalog");
        let durable = durable(root.path());
        let registry = CatalogRegistry::desktop(root.path()).expect("desktop registry");
        registry.bootstrap(&durable).expect("bootstrap");
        let registration = |id: &str, public_key: String| CatalogSourceRegistration {
            id: id.to_string(),
            url: "catalog.json".to_string(),
            required_key_id: "fixture-key".to_string(),
            auth_secret_ref: String::new(),
            enabled: true,
            offline_oci_layouts: BTreeMap::new(),
            public_key: Some(public_key),
        };
        registry
            .register_source(
                &durable,
                registration(
                    "first",
                    STANDARD.encode(signing_key.verifying_key().to_bytes()),
                ),
            )
            .expect("first binding");
        let other_key = SigningKey::from_bytes(&[10_u8; 32]);
        let error = registry
            .register_source(
                &durable,
                registration(
                    "second",
                    STANDARD.encode(other_key.verifying_key().to_bytes()),
                ),
            )
            .expect_err("key ID rebinding must fail");
        assert_eq!(error.code(), "CATALOG_TRUST_KEY_CONFLICT");
        assert_eq!(
            registry
                .source_page(&durable, None, None)
                .expect("sources")
                .items
                .len(),
            1
        );
    }
}
