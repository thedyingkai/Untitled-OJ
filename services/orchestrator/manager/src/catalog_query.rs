//! Read-only Catalog use cases. HTTP, Console, trust I/O and storage stay in adapters.
use crate::InstalledServiceView;
use crate::catalog_v2::{ReleaseChannel, RuntimeCapabilityV2, TargetPlatform};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CatalogSource {
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

#[derive(Debug, Clone, Default)]
pub struct PackageQuery {
    pub search: Option<String>,
    pub channel: Option<ReleaseChannel>,
    pub platform: Option<TargetPlatform>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogPackageItem {
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
pub struct PackagePage {
    pub items: Vec<CatalogPackageItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogSourcePage {
    pub items: Vec<CatalogSource>,
    pub next_cursor: Option<String>,
}

fn default_enabled() -> bool {
    true
}

/// Exposes reads only; source registration and trust mutation are not query capabilities.
pub trait CatalogReadPort {
    type Error;

    fn source_page(
        &self,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<CatalogSourcePage, Self::Error>;

    fn packages(&self, query: &PackageQuery) -> Result<PackagePage, Self::Error>;
}

/// Host-owned projection of deployed services. The use case does not know its storage.
pub trait InstalledServicesReadPort {
    type Error;

    fn installed_services(&self) -> Result<BTreeMap<String, InstalledServiceView>, Self::Error>;
}

#[derive(Debug)]
pub enum CatalogQueryError<CatalogError, InstallationError> {
    Catalog(CatalogError),
    Installations(InstallationError),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CatalogPackageList {
    pub items: Vec<CatalogPackageItem>,
    pub installed: BTreeMap<String, InstalledServiceView>,
    pub next_cursor: Option<String>,
}

/// Keep the established failure order: verify/read the Catalog before consulting deployments.
/// Neither a failed Catalog nor a failed deployment projection becomes an empty success.
pub fn list_packages<C, I>(
    catalog: &C,
    installations: &I,
    query: &PackageQuery,
) -> Result<CatalogPackageList, CatalogQueryError<C::Error, I::Error>>
where
    C: CatalogReadPort + ?Sized,
    I: InstalledServicesReadPort + ?Sized,
{
    let page = catalog
        .packages(query)
        .map_err(CatalogQueryError::Catalog)?;
    let installed = installations
        .installed_services()
        .map_err(CatalogQueryError::Installations)?;
    Ok(CatalogPackageList {
        items: page.items,
        installed,
        next_cursor: page.next_cursor,
    })
}
