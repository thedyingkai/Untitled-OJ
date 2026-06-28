use crate::{InstallerError, Manifest, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledModule {
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub status: ModuleState,
    pub kind: String,
    #[serde(default)]
    pub manifest: Option<Manifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModuleState {
    Enabled,
    Disabled,
    #[default]
    Installed,
    FailedInstall,
    FailedUpgrade,
    Removed,
}

impl ModuleState {
    pub fn is_enabled(&self) -> bool {
        matches!(self, ModuleState::Enabled)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistrySnapshot {
    #[serde(default)]
    pub modules: Vec<InstalledModule>,
}

impl RegistrySnapshot {
    pub fn by_id(&self) -> HashMap<&str, &InstalledModule> {
        self.modules
            .iter()
            .map(|item| (item.module_id.as_str(), item))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanRequest {
    pub manifest: Manifest,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub kind: PlanKind,
    pub module_id: String,
    pub version: String,
    pub dry_run: bool,
    pub can_apply: bool,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub affected_tables: Vec<String>,
    #[serde(default)]
    pub affected_modules: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<PlanWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanKind {
    Install,
    Enable,
    Disable,
    Upgrade,
    Rollback,
    Uninstall,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanWarning {
    pub code: String,
    pub message: String,
}

pub fn install_plan(
    manifest: &Manifest,
    snapshot: &RegistrySnapshot,
    dry_run: bool,
) -> Result<Plan> {
    let mut plan = base_plan(PlanKind::Install, manifest, dry_run);
    plan.affected_tables = registry_tables();
    plan.dependencies = manifest
        .requires
        .modules
        .iter()
        .map(|dep| dep.id.clone())
        .collect();

    check_dependencies(manifest, snapshot, &mut plan)?;
    check_cycles(snapshot, &mut plan, manifest)?;

    let modules = snapshot.by_id();
    if let Some(existing) = modules.get(manifest.id.as_str()) {
        if existing.version == manifest.version {
            plan.actions.push(Action {
                action: "idempotent_update".to_string(),
                target: manifest.id.clone(),
                detail: "same version already installed; metadata will be refreshed".to_string(),
            });
        } else {
            plan.actions.push(Action {
                action: "install_new_version_metadata".to_string(),
                target: manifest.id.clone(),
                detail: format!(
                    "existing version {} will be updated to {}",
                    existing.version, manifest.version
                ),
            });
        }
    } else {
        plan.actions.push(Action {
            action: "insert_module_metadata".to_string(),
            target: manifest.id.clone(),
            detail: "create module registry and installation rows".to_string(),
        });
    }

    plan.actions.extend(metadata_actions(manifest));
    finalize_plan(plan)
}

pub fn enable_plan(module_id: &str, snapshot: &RegistrySnapshot, dry_run: bool) -> Result<Plan> {
    let mut plan = base_plan_for_id(PlanKind::Enable, module_id, "", dry_run);
    plan.affected_tables = vec![
        "module_nodes".to_string(),
        "module_installations".to_string(),
        "module_operations".to_string(),
    ];
    let modules = snapshot.by_id();
    let Some(module) = modules.get(module_id) else {
        plan.blocked_by.push("module is not installed".to_string());
        return finalize_plan(plan);
    };
    plan.version = module.version.clone();
    if module.kind == "kernel" {
        plan.warnings.push(warning(
            "kernel_always_enabled",
            "kernel modules are expected to remain enabled",
        ));
    }
    if module.status.is_enabled() {
        plan.actions.push(Action {
            action: "noop".to_string(),
            target: module_id.to_string(),
            detail: "module already enabled".to_string(),
        });
    } else {
        if let Some(manifest) = &module.manifest {
            check_dependencies(manifest, snapshot, &mut plan)?;
        }
        plan.actions.push(Action {
            action: "set_enabled".to_string(),
            target: module_id.to_string(),
            detail: "mark module node and installation ENABLED".to_string(),
        });
    }
    finalize_plan(plan)
}

pub fn disable_plan(module_id: &str, snapshot: &RegistrySnapshot, dry_run: bool) -> Result<Plan> {
    let mut plan = base_plan_for_id(PlanKind::Disable, module_id, "", dry_run);
    plan.affected_tables = vec![
        "module_nodes".to_string(),
        "module_installations".to_string(),
        "module_operations".to_string(),
    ];
    let modules = snapshot.by_id();
    let Some(module) = modules.get(module_id) else {
        plan.blocked_by.push("module is not installed".to_string());
        return finalize_plan(plan);
    };
    plan.version = module.version.clone();
    protect_disable(module_id, module, &mut plan);
    for dependent in enabled_dependents(module_id, snapshot) {
        plan.blocked_by
            .push(format!("enabled dependent {}", dependent));
    }
    if plan.blocked_by.is_empty() {
        plan.actions.push(Action {
            action: "set_disabled".to_string(),
            target: module_id.to_string(),
            detail: "mark module node and installation DISABLED".to_string(),
        });
    }
    finalize_plan(plan)
}

pub fn upgrade_plan(
    old: Option<&Manifest>,
    new: &Manifest,
    snapshot: &RegistrySnapshot,
    dry_run: bool,
) -> Result<Plan> {
    let mut plan = base_plan(PlanKind::Upgrade, new, dry_run);
    plan.affected_tables = registry_tables();
    check_dependencies(new, snapshot, &mut plan)?;
    if let Some(old) = old {
        let old_v = Version::parse(&old.version).map_err(|_| {
            InstallerError::InvalidManifest("old manifest version must be semver".to_string())
        })?;
        let new_v = Version::parse(&new.version).map_err(|_| {
            InstallerError::InvalidManifest("new manifest version must be semver".to_string())
        })?;
        if new_v <= old_v {
            plan.blocked_by
                .push("upgrade version must increase".to_string());
        }
        plan.actions.extend(diff_manifests(old, new));
    } else {
        plan.warnings.push(warning(
            "missing_old_manifest",
            "old manifest unavailable; generating install-like upgrade plan",
        ));
        plan.actions.extend(metadata_actions(new));
    }
    finalize_plan(plan)
}

pub fn rollback_plan(module_id: &str, snapshot: &RegistrySnapshot, dry_run: bool) -> Result<Plan> {
    let mut plan = base_plan_for_id(PlanKind::Rollback, module_id, "", dry_run);
    plan.affected_tables = registry_tables();
    let modules = snapshot.by_id();
    if let Some(module) = modules.get(module_id) {
        plan.version = module.version.clone();
        if module.kind == "kernel" || module.kind == "platform" || module_id == "ojos.judge-core" {
            plan.blocked_by
                .push("protected module rollback is dry-run only in v0".to_string());
        }
        plan.actions.push(Action {
            action: "metadata_rollback_plan".to_string(),
            target: module_id.to_string(),
            detail: "derive rollback from module_operations history; apply is demo-only in v0"
                .to_string(),
        });
    } else {
        plan.blocked_by.push("module is not installed".to_string());
    }
    finalize_plan(plan)
}

pub fn uninstall_plan(module_id: &str, snapshot: &RegistrySnapshot, dry_run: bool) -> Result<Plan> {
    let mut plan = base_plan_for_id(PlanKind::Uninstall, module_id, "", dry_run);
    plan.affected_tables = registry_tables();
    let modules = snapshot.by_id();
    let Some(module) = modules.get(module_id) else {
        plan.blocked_by.push("module is not installed".to_string());
        return finalize_plan(plan);
    };
    plan.version = module.version.clone();
    if module.kind == "kernel" || module.kind == "platform" || module_id == "ojos.judge-core" {
        plan.blocked_by
            .push("protected builtin module cannot be uninstalled".to_string());
    }
    if module.kind == "feature"
        && module.manifest.as_ref().map(|m| m.status.as_str()) == Some("builtin")
    {
        plan.blocked_by
            .push("builtin module cannot be uninstalled".to_string());
    }
    for dependent in enabled_dependents(module_id, snapshot) {
        plan.blocked_by
            .push(format!("enabled dependent {}", dependent));
    }
    plan.actions.push(Action {
        action: "uninstall_metadata".to_string(),
        target: module_id.to_string(),
        detail: "remove metadata rows; apply is only allowed for demo modules in v0".to_string(),
    });
    finalize_plan(plan)
}

pub fn diff_manifests(old: &Manifest, new: &Manifest) -> Vec<Action> {
    let mut actions = Vec::new();
    diff_set(
        &mut actions,
        "permissions",
        old.provides
            .permissions
            .iter()
            .map(|p| p.key.as_str())
            .collect(),
        new.provides
            .permissions
            .iter()
            .map(|p| p.key.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "components",
        old.provides
            .components
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
        new.provides
            .components
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "roles",
        old.provides.roles.iter().map(|p| p.key.as_str()).collect(),
        new.provides.roles.iter().map(|p| p.key.as_str()).collect(),
    );
    diff_set(
        &mut actions,
        "services",
        old.provides
            .services
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
        new.provides
            .services
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "workers",
        old.provides.workers.iter().map(|p| p.id.as_str()).collect(),
        new.provides.workers.iter().map(|p| p.id.as_str()).collect(),
    );
    diff_set(
        &mut actions,
        "frontend_routes",
        old.provides
            .frontend_routes
            .iter()
            .map(|p| p.path.as_str())
            .collect(),
        new.provides
            .frontend_routes
            .iter()
            .map(|p| p.path.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "menus",
        old.provides.menus.iter().map(|p| p.key.as_str()).collect(),
        new.provides.menus.iter().map(|p| p.key.as_str()).collect(),
    );
    diff_set(
        &mut actions,
        "gateway_routes",
        old.provides
            .gateway_routes
            .iter()
            .map(|p| p.prefix.as_str())
            .collect(),
        new.provides
            .gateway_routes
            .iter()
            .map(|p| p.prefix.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "storage_buckets",
        old.provides
            .storage_buckets
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
        new.provides
            .storage_buckets
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "health_checks",
        old.provides
            .health_checks
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
        new.provides
            .health_checks
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
    );
    diff_set(
        &mut actions,
        "admin_panels",
        old.provides
            .admin_panels
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
        new.provides
            .admin_panels
            .iter()
            .map(|p| p.id.as_str())
            .collect(),
    );
    if actions.is_empty() {
        actions.push(Action {
            action: "metadata_version_update".to_string(),
            target: new.id.clone(),
            detail: "no manifest surface changes detected".to_string(),
        });
    }
    actions
}

fn check_dependencies(
    manifest: &Manifest,
    snapshot: &RegistrySnapshot,
    plan: &mut Plan,
) -> Result<()> {
    let modules = snapshot.by_id();
    for dep in &manifest.requires.modules {
        match modules.get(dep.id.as_str()) {
            Some(installed) => {
                if !installed.status.is_enabled() {
                    plan.blocked_by
                        .push(format!("dependency {} is not enabled", dep.id));
                }
                if !version_satisfies(&installed.version, &dep.version)? {
                    plan.blocked_by
                        .push(format!("dependency {} version mismatch", dep.id));
                }
            }
            None => plan
                .blocked_by
                .push(format!("missing dependency {}", dep.id)),
        }
    }
    Ok(())
}

fn check_cycles(snapshot: &RegistrySnapshot, plan: &mut Plan, manifest: &Manifest) -> Result<()> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for module in &snapshot.modules {
        if let Some(existing) = &module.manifest {
            graph.insert(
                existing.id.clone(),
                existing
                    .requires
                    .modules
                    .iter()
                    .map(|d| d.id.clone())
                    .collect(),
            );
        }
    }
    graph.insert(
        manifest.id.clone(),
        manifest
            .requires
            .modules
            .iter()
            .map(|d| d.id.clone())
            .collect(),
    );
    if has_cycle(&graph) {
        plan.blocked_by
            .push("cycle dependency detected".to_string());
    }
    Ok(())
}

fn enabled_dependents(module_id: &str, snapshot: &RegistrySnapshot) -> Vec<String> {
    snapshot
        .modules
        .iter()
        .filter(|module| module.status.is_enabled())
        .filter_map(|module| {
            let manifest = module.manifest.as_ref()?;
            if manifest
                .requires
                .modules
                .iter()
                .any(|dep| dep.id == module_id)
            {
                Some(module.module_id.clone())
            } else {
                None
            }
        })
        .collect()
}

fn protect_disable(module_id: &str, module: &InstalledModule, plan: &mut Plan) {
    if module.kind == "kernel" {
        plan.blocked_by
            .push("kernel module cannot be disabled".to_string());
    }
    if module.kind == "platform" {
        plan.blocked_by
            .push("platform module is protected by default".to_string());
    }
    if module_id == "ojos.judge-core" {
        plan.blocked_by
            .push("judge-core disable is protected by default".to_string());
    }
}

fn version_satisfies(actual: &str, constraint: &str) -> Result<bool> {
    let actual = Version::parse(actual).map_err(|_| {
        InstallerError::Dependency("installed module version is not semver".to_string())
    })?;
    let constraint = constraint.trim();
    if constraint.is_empty() {
        return Ok(true);
    }
    if let Some(want) = constraint.strip_prefix(">=") {
        return Ok(actual
            >= Version::parse(want.trim()).map_err(|_| {
                InstallerError::Dependency("invalid dependency constraint".to_string())
            })?);
    }
    if let Some(want) = constraint.strip_prefix('=') {
        return Ok(actual
            == Version::parse(want.trim()).map_err(|_| {
                InstallerError::Dependency("invalid dependency constraint".to_string())
            })?);
    }
    Ok(actual
        == Version::parse(constraint)
            .map_err(|_| InstallerError::Dependency("invalid dependency constraint".to_string()))?)
}

fn has_cycle(graph: &HashMap<String, Vec<String>>) -> bool {
    fn visit(
        node: &str,
        graph: &HashMap<String, Vec<String>>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) -> bool {
        if visiting.contains(node) {
            return true;
        }
        if visited.contains(node) {
            return false;
        }
        visiting.insert(node.to_string());
        if let Some(edges) = graph.get(node) {
            for next in edges {
                if graph.contains_key(next) && visit(next, graph, visiting, visited) {
                    return true;
                }
            }
        }
        visiting.remove(node);
        visited.insert(node.to_string());
        false
    }

    let mut visited = HashSet::new();
    for node in graph.keys() {
        let mut visiting = HashSet::new();
        if visit(node, graph, &mut visiting, &mut visited) {
            return true;
        }
    }
    false
}

fn base_plan(kind: PlanKind, manifest: &Manifest, dry_run: bool) -> Plan {
    base_plan_for_id(kind, &manifest.id, &manifest.version, dry_run)
}

fn base_plan_for_id(kind: PlanKind, module_id: &str, version: &str, dry_run: bool) -> Plan {
    Plan {
        kind,
        module_id: module_id.to_string(),
        version: version.to_string(),
        dry_run,
        can_apply: false,
        actions: Vec::new(),
        affected_tables: Vec::new(),
        affected_modules: vec![module_id.to_string()],
        dependencies: Vec::new(),
        blocked_by: Vec::new(),
        warnings: Vec::new(),
    }
}

fn finalize_plan(mut plan: Plan) -> Result<Plan> {
    plan.affected_modules.sort();
    plan.affected_modules.dedup();
    plan.dependencies.sort();
    plan.dependencies.dedup();
    plan.blocked_by.sort();
    plan.blocked_by.dedup();
    plan.can_apply = plan.blocked_by.is_empty();
    Ok(plan)
}

fn registry_tables() -> Vec<String> {
    [
        "module_installations",
        "module_nodes",
        "module_edges",
        "module_components",
        "module_permissions",
        "module_menus",
        "module_frontend_routes",
        "module_gateway_routes",
        "module_operations",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn metadata_actions(manifest: &Manifest) -> Vec<Action> {
    let mut actions = vec![
        Action {
            action: "upsert_module_node".to_string(),
            target: manifest.id.clone(),
            detail: manifest.name.clone(),
        },
        Action {
            action: "upsert_installation".to_string(),
            target: manifest.id.clone(),
            detail: manifest.version.clone(),
        },
    ];
    for dep in &manifest.requires.modules {
        actions.push(Action {
            action: "upsert_dependency_edge".to_string(),
            target: dep.id.clone(),
            detail: dep.version.clone(),
        });
    }
    for permission in &manifest.provides.permissions {
        actions.push(Action {
            action: "upsert_permission".to_string(),
            target: permission.key.clone(),
            detail: permission.description.clone(),
        });
    }
    for component in &manifest.provides.components {
        actions.push(Action {
            action: "upsert_component".to_string(),
            target: component.id.clone(),
            detail: component.component_type.clone(),
        });
    }
    for menu in &manifest.provides.menus {
        actions.push(Action {
            action: "upsert_menu".to_string(),
            target: menu.key.clone(),
            detail: menu.route_path.clone(),
        });
    }
    for route in &manifest.provides.frontend_routes {
        actions.push(Action {
            action: "upsert_frontend_route".to_string(),
            target: route.path.clone(),
            detail: route.component_key.clone(),
        });
    }
    for route in &manifest.provides.gateway_routes {
        actions.push(Action {
            action: "upsert_gateway_route".to_string(),
            target: route.prefix.clone(),
            detail: route.target_service.clone(),
        });
    }
    actions
}

fn diff_set(actions: &mut Vec<Action>, label: &str, old: HashSet<&str>, new: HashSet<&str>) {
    for added in new.difference(&old) {
        actions.push(Action {
            action: format!("add_{}", label),
            target: (*added).to_string(),
            detail: String::new(),
        });
    }
    for removed in old.difference(&new) {
        actions.push(Action {
            action: format!("remove_{}", label),
            target: (*removed).to_string(),
            detail: String::new(),
        });
    }
}

fn warning(code: &str, message: &str) -> PlanWarning {
    PlanWarning {
        code: code.to_string(),
        message: message.to_string(),
    }
}
