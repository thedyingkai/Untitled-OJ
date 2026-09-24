//! Catalog read ports backed by the trusted registry and the existing deployment projection.
//! Queries receive a repository-only application context, never an action console.

use crate::catalog_registry::{CatalogRegistry, CatalogRegistryError};
use crate::durable::DurableStore;
use crate::registry::RegistryContext;
use orchestrator_manager::InstalledServiceView;
use orchestrator_manager::catalog_query::{
    CatalogReadPort, CatalogSourcePage, InstalledServicesReadPort, PackagePage, PackageQuery,
};
use std::collections::BTreeMap;

pub(crate) struct CatalogRegistryReader<'a> {
    registry: &'a CatalogRegistry,
    storage: &'a DurableStore,
}

impl<'a> CatalogRegistryReader<'a> {
    pub(crate) fn new(registry: &'a CatalogRegistry, storage: &'a DurableStore) -> Self {
        Self { registry, storage }
    }
}

impl CatalogReadPort for CatalogRegistryReader<'_> {
    type Error = CatalogRegistryError;

    fn source_page(
        &self,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<CatalogSourcePage, Self::Error> {
        self.registry.source_page(self.storage, cursor, limit)
    }

    fn packages(&self, query: &PackageQuery) -> Result<PackagePage, Self::Error> {
        self.registry.packages(self.storage, query)
    }
}

pub(crate) struct InstalledServicesReader<'a> {
    registry_context: &'a RegistryContext,
}

impl<'a> InstalledServicesReader<'a> {
    pub(crate) fn new(registry_context: &'a RegistryContext) -> Self {
        Self { registry_context }
    }
}

impl InstalledServicesReadPort for InstalledServicesReader<'_> {
    type Error = anyhow::Error;

    fn installed_services(&self) -> Result<BTreeMap<String, InstalledServiceView>, Self::Error> {
        self.registry_context.installed_services()
    }
}
