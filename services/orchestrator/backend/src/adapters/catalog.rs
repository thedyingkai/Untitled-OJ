//! Catalog read ports backed by the trusted registry and the existing deployment projection.
//! The Console dependency is transitional and deliberately hidden from query use cases.

use crate::catalog_registry::{CatalogRegistry, CatalogRegistryError};
use crate::durable::DurableStore;
use orchestrator_legacy::OrchestratorActionConsole;
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
    console: &'a OrchestratorActionConsole,
}

impl<'a> InstalledServicesReader<'a> {
    pub(crate) fn new(console: &'a OrchestratorActionConsole) -> Self {
        Self { console }
    }
}

impl InstalledServicesReadPort for InstalledServicesReader<'_> {
    type Error = anyhow::Error;

    fn installed_services(&self) -> Result<BTreeMap<String, InstalledServiceView>, Self::Error> {
        orchestrator_manager::installed_services(self.console)
    }
}
