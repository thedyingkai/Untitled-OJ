mod package;
mod service;

pub use package::{PackageMetadata, PackageVerification, package_service, verify_package};
pub use service::{
    DeviceKind, EndpointDecl, LinkDecl, LinkEndpointRef, RuntimeMode, ServiceManifest, ServicePlan,
    ServicePlanAction, ServicePlanKind, ServiceRuntimeDecl, ServiceSecurityDecl, ServiceSet,
    ServiceSetLink, ServiceState, ServiceUiDecl, SetExpandResult, TopologySnapshot, expand_set,
    service_install_plan, validate_endpoint_id, validate_service_manifest,
    validate_service_manifest_file, validate_service_set, validate_service_set_file,
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
