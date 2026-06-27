use crate::{InstallerError, Result, sanitize_path_for_error};
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path};

const SUPPORTED_SCHEMA_VERSION: u32 = 1;
const DESCRIPTION_MAX: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub set: String,
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub compatibility: Compatibility,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub provides: Provides,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub signing_key_id: Option<String>,
    #[serde(default)]
    pub trusted_publisher: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub installer: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    #[serde(default)]
    pub modules: Vec<ModuleDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModuleDependency {
    pub id: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provides {
    #[serde(default)]
    pub permissions: Vec<PermissionDecl>,
    #[serde(default)]
    pub components: Vec<ComponentDecl>,
    #[serde(default)]
    pub frontend_routes: Vec<FrontendRouteDecl>,
    #[serde(default)]
    pub menus: Vec<MenuDecl>,
    #[serde(default)]
    pub gateway_routes: Vec<GatewayRouteDecl>,
    #[serde(default)]
    pub storage: StorageDecl,
    #[serde(default)]
    pub health_checks: Vec<HealthCheckDecl>,
    #[serde(default)]
    pub migrations: Vec<MigrationDecl>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionDecl {
    pub key: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComponentDecl {
    pub id: String,
    #[serde(rename = "type")]
    pub component_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrontendRouteDecl {
    pub path: String,
    pub name: String,
    pub component_key: String,
    #[serde(default)]
    pub required_permission: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MenuDecl {
    pub key: String,
    pub title: String,
    pub route_path: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub parent_key: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub required_permission: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GatewayRouteDecl {
    pub prefix: String,
    pub target_service: String,
    pub auth_mode: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StorageDecl {
    #[serde(default)]
    pub buckets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HealthCheckDecl {
    pub id: String,
    #[serde(rename = "type")]
    pub check_type: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MigrationDecl {
    pub up: String,
    pub down: String,
}

pub fn validate_manifest_file(repo_root: &Path, manifest_path: &Path) -> Result<Manifest> {
    validate_manifest_path(repo_root, manifest_path)?;
    let text = fs::read_to_string(repo_root.join(manifest_path))?;
    let manifest: Manifest = serde_yaml::from_str(&text)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
    let id_re = Regex::new(r"^[a-z0-9][a-z0-9.-]*$").expect("valid regex");
    let key_re = Regex::new(r"^[a-z0-9][a-z0-9_.:-]*$").expect("valid regex");
    let path_re = Regex::new(r"^/[A-Za-z0-9_./:{}-]*$").expect("valid regex");

    ensure(
        manifest.schema_version == SUPPORTED_SCHEMA_VERSION,
        "unsupported schema_version",
    )?;
    ensure(id_re.is_match(manifest.id.trim()), "id format is invalid")?;
    ensure(!manifest.name.trim().is_empty(), "name is required")?;
    Version::parse(manifest.version.trim())
        .map_err(|_| InstallerError::InvalidManifest("version must be semver".to_string()))?;
    ensure(!manifest.set.trim().is_empty(), "set is required")?;
    ensure(
        matches!(
            manifest.kind.as_str(),
            "kernel" | "feature" | "integration" | "metadata"
        ),
        "kind is invalid",
    )?;
    ensure(
        matches!(manifest.status.as_str(), "builtin" | "external" | "demo"),
        "status is invalid",
    )?;
    ensure(
        manifest.description.len() <= DESCRIPTION_MAX,
        "description is too long",
    )?;

    let raw = serde_json::to_value(manifest)?;
    reject_dangerous_keys(&raw)?;

    unique_by(
        manifest.provides.permissions.iter().map(|p| p.key.as_str()),
        "duplicate permission",
    )?;
    for item in &manifest.provides.permissions {
        ensure(key_re.is_match(&item.key), "permission key is invalid")?;
    }

    unique_by(
        manifest.provides.components.iter().map(|c| c.id.as_str()),
        "duplicate component",
    )?;
    for item in &manifest.provides.components {
        ensure(key_re.is_match(&item.id), "component id is invalid")?;
        ensure(
            !item.component_type.trim().is_empty(),
            "component type is required",
        )?;
    }

    unique_by(
        manifest
            .provides
            .frontend_routes
            .iter()
            .map(|r| r.path.as_str()),
        "duplicate frontend route",
    )?;
    for item in &manifest.provides.frontend_routes {
        ensure(
            path_re.is_match(&item.path),
            "frontend route path is invalid",
        )?;
        ensure(
            !item.name.trim().is_empty(),
            "frontend route name is required",
        )?;
        ensure(
            !item.component_key.trim().is_empty(),
            "frontend component_key is required",
        )?;
    }

    unique_by(
        manifest.provides.menus.iter().map(|m| m.key.as_str()),
        "duplicate menu",
    )?;
    for item in &manifest.provides.menus {
        ensure(key_re.is_match(&item.key), "menu key is invalid")?;
        ensure(
            path_re.is_match(&item.route_path),
            "menu route_path is invalid",
        )?;
    }

    unique_by(
        manifest
            .provides
            .gateway_routes
            .iter()
            .map(|r| r.prefix.as_str()),
        "duplicate gateway prefix",
    )?;
    for item in &manifest.provides.gateway_routes {
        ensure(path_re.is_match(&item.prefix), "gateway prefix is invalid")?;
        ensure(
            !item.target_service.trim().is_empty(),
            "gateway target_service is required",
        )?;
        ensure(
            matches!(
                item.auth_mode.as_str(),
                "none" | "optional" | "required" | "user" | "worker"
            ),
            "gateway auth_mode is invalid",
        )?;
    }

    unique_by(
        manifest.provides.storage.buckets.iter().map(|b| b.as_str()),
        "duplicate storage bucket",
    )?;
    for item in &manifest.provides.storage.buckets {
        ensure(key_re.is_match(item), "storage bucket is invalid")?;
    }

    unique_by(
        manifest
            .provides
            .health_checks
            .iter()
            .map(|h| h.id.as_str()),
        "duplicate health check",
    )?;
    for item in &manifest.provides.health_checks {
        ensure(key_re.is_match(&item.id), "health check id is invalid")?;
    }

    unique_by(
        manifest.requires.modules.iter().map(|d| d.id.as_str()),
        "duplicate required module",
    )?;
    for dep in &manifest.requires.modules {
        ensure(id_re.is_match(&dep.id), "dependency id is invalid")?;
        ensure(dep.id != manifest.id, "self dependency is not allowed")?;
        validate_version_constraint(&dep.version)?;
    }

    for migration in &manifest.provides.migrations {
        validate_migration_path_pair(&migration.up, &migration.down)?;
    }

    Ok(())
}

pub fn validate_manifest_path(repo_root: &Path, manifest_path: &Path) -> Result<()> {
    ensure(
        !manifest_path.is_absolute(),
        "manifest path must be relative",
    )?;
    reject_path_components(manifest_path)?;
    let modules_dir = repo_root.join("modules");
    let full = repo_root.join(manifest_path);
    let canonical_modules = modules_dir.canonicalize().map_err(|_| {
        InstallerError::UnsafePath("modules directory is not available".to_string())
    })?;
    let canonical_manifest = full
        .canonicalize()
        .map_err(|_| InstallerError::UnsafePath(sanitize_path_for_error(manifest_path)))?;
    ensure(
        canonical_manifest.starts_with(&canonical_modules),
        "manifest must stay under modules",
    )?;
    ensure(
        manifest_path.file_name().and_then(|v| v.to_str()) == Some("module.yaml"),
        "manifest file must be module.yaml",
    )?;
    Ok(())
}

pub fn validate_package_entry_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    ensure(!path.is_absolute(), "package path must be relative")?;
    reject_path_components(path)?;
    Ok(())
}

fn validate_migration_path_pair(up: &str, down: &str) -> Result<()> {
    validate_migration_path(up)?;
    validate_migration_path(down)?;
    ensure(
        up.ends_with(".up.sql") && down.ends_with(".down.sql"),
        "migration up/down suffix is invalid",
    )?;
    let up_base = up.trim_end_matches(".up.sql");
    let down_base = down.trim_end_matches(".down.sql");
    ensure(
        up_base == down_base,
        "migration up/down pair does not match",
    )?;
    Ok(())
}

fn validate_migration_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    ensure(!p.is_absolute(), "migration path must be relative")?;
    reject_path_components(p)?;
    ensure(
        path.starts_with("deploy/migrations/"),
        "migration path must be under deploy/migrations",
    )?;
    ensure(path.ends_with(".sql"), "migration path must be .sql")?;
    Ok(())
}

fn reject_path_components(path: &Path) -> Result<()> {
    let banned = [".tmp", ".env", "node_modules", "dist", "target", ".git"];
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(InstallerError::UnsafePath(
                    "path traversal is not allowed".to_string(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(InstallerError::UnsafePath(
                    "absolute path is not allowed".to_string(),
                ));
            }
            Component::Normal(value) => {
                let text = value.to_string_lossy();
                if banned.iter().any(|item| text.eq_ignore_ascii_case(item)) {
                    return Err(InstallerError::UnsafePath(format!(
                        "banned path segment {}",
                        text
                    )));
                }
            }
            Component::CurDir => {}
        }
    }
    Ok(())
}

fn validate_version_constraint(value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    for prefix in [">=", "=", ">", "<=", "<", "^"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            Version::parse(rest.trim()).map_err(|_| {
                InstallerError::InvalidManifest(format!("invalid version constraint {}", value))
            })?;
            return Ok(());
        }
    }
    Version::parse(value).map_err(|_| {
        InstallerError::InvalidManifest(format!("invalid version constraint {}", value))
    })?;
    Ok(())
}

fn reject_dangerous_keys(value: &Value) -> Result<()> {
    let banned = [
        "secret",
        "token",
        "password",
        "private_key",
        "env",
        "command",
        "script",
        "hook",
        "postinstall",
        "preinstall",
        "remote_url",
        "download_url",
    ];
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                if banned.iter().any(|item| lower == *item) {
                    return Err(InstallerError::InvalidManifest(format!(
                        "dangerous field {} is not allowed",
                        key
                    )));
                }
                reject_dangerous_keys(child)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_dangerous_keys(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn unique_by<'a>(items: impl Iterator<Item = &'a str>, msg: &str) -> Result<()> {
    let mut seen = HashSet::new();
    for item in items {
        let key = item.trim();
        ensure(!key.is_empty(), msg)?;
        if !seen.insert(key.to_string()) {
            return Err(InstallerError::InvalidManifest(msg.to_string()));
        }
    }
    Ok(())
}

fn ensure(ok: bool, msg: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(InstallerError::InvalidManifest(msg.to_string()))
    }
}
