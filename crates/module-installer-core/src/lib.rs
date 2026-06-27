mod manifest;
mod package;
mod plan;

pub use manifest::{
    Compatibility, ComponentDecl, FrontendRouteDecl, GatewayRouteDecl, HealthCheckDecl, Manifest,
    MenuDecl, MigrationDecl, ModuleDependency, PermissionDecl, Provides, StorageDecl,
    validate_manifest, validate_manifest_file,
};
pub use package::{PackageMetadata, PackageVerification, package_module, verify_package};
pub use plan::{
    Action, InstalledModule, ModuleState, Plan, PlanKind, PlanRequest, PlanWarning,
    RegistrySnapshot, diff_manifests, disable_plan, enable_plan, install_plan, rollback_plan,
    uninstall_plan, upgrade_plan,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error("dependency error: {0}")]
    Dependency(String),
    #[error("operation blocked: {0}")]
    Blocked(String),
    #[error("package verification failed: {0}")]
    Package(String),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

pub type Result<T> = std::result::Result<T, InstallerError>;

pub fn sanitize_path_for_error(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("path")
        .to_string()
}

#[cfg(test)]
mod tests;
