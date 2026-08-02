use crate::{OrchestratorError, Result, SharedSchemas};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionRisk {
    Low,
    Medium,
    High,
}

impl ActionRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionPlanMode {
    ReadOnly,
    Direct,
    Planned,
    ConfirmedPlan,
}

impl ActionPlanMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "readonly",
            Self::Direct => "direct",
            Self::Planned => "plan",
            Self::ConfirmedPlan => "plan+confirm",
        }
    }

    pub fn plan_requirement(self) -> &'static str {
        match self {
            Self::ReadOnly => "no plan required",
            Self::Direct => "no plan required",
            Self::Planned => "plan required",
            Self::ConfirmedPlan => "confirmation required",
        }
    }

    pub fn requires_plan(self) -> bool {
        matches!(self, Self::Planned | Self::ConfirmedPlan)
    }

    pub fn requires_confirmation(self) -> bool {
        matches!(self, Self::ConfirmedPlan)
    }
}

#[derive(Debug, Copy, Clone, Serialize, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub action: &'static str,
    pub target_type: &'static str,
    pub risk: ActionRisk,
    pub plan_mode: ActionPlanMode,
    pub summary: &'static str,
}

impl ActionDescriptor {
    pub fn risk_label(self) -> &'static str {
        self.risk.label()
    }

    pub fn mode_label(self) -> &'static str {
        self.plan_mode.label()
    }

    pub fn plan_requirement(self) -> &'static str {
        self.plan_mode.plan_requirement()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct ActionLayer {
    prefix: &'static str,
    target_type: &'static str,
    requires_crud: bool,
}

pub const CORE_ACTION_TARGETS: &[&str] = &[
    "ServiceRelease",
    "Host",
    "Service",
    "Endpoint",
    "Link",
    "Route",
    "FrontendEntry",
    "Migration",
    "Permission",
    "RedisResource",
    "StorageResource",
    "Config",
    "Secret",
    "Topology",
    "Operation",
    "LogView",
    "DiagnosticReport",
];

const ACTION_LAYERS: &[ActionLayer] = &[
    ActionLayer {
        prefix: "release.",
        target_type: "ServiceRelease",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "host.",
        target_type: "Host",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "service.",
        target_type: "Service",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "endpoint.",
        target_type: "Endpoint",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "link.",
        target_type: "Link",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "route.",
        target_type: "Route",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "frontend.",
        target_type: "FrontendEntry",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "migration.",
        target_type: "Migration",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "permission.",
        target_type: "Permission",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "redis.",
        target_type: "RedisResource",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "storage.",
        target_type: "StorageResource",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "config.",
        target_type: "Config",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "secret.",
        target_type: "Secret",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "topology.",
        target_type: "Topology",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "operation.",
        target_type: "Operation",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "log.",
        target_type: "LogView",
        requires_crud: true,
    },
    ActionLayer {
        prefix: "diagnostic.",
        target_type: "DiagnosticReport",
        requires_crud: true,
    },
];

pub const FORMAL_ACTION_PREFIXES: &[&str] = &[
    "release.",
    "host.",
    "service.",
    "endpoint.",
    "link.",
    "route.",
    "frontend.",
    "migration.",
    "permission.",
    "redis.",
    "storage.",
    "config.",
    "secret.",
    "topology.",
    "operation.",
    "log.",
    "diagnostic.",
];

macro_rules! action {
    ($action:literal, $target:literal, $risk:ident, $mode:ident, $summary:literal) => {
        ActionDescriptor {
            action: $action,
            target_type: $target,
            risk: ActionRisk::$risk,
            plan_mode: ActionPlanMode::$mode,
            summary: $summary,
        }
    };
}

pub const ACTION_CATALOG: &[ActionDescriptor] = &[
    action!(
        "release.create",
        "ServiceRelease",
        Medium,
        Planned,
        "register a service release manifest"
    ),
    action!(
        "release.list",
        "ServiceRelease",
        Low,
        ReadOnly,
        "list known service releases"
    ),
    action!(
        "release.get",
        "ServiceRelease",
        Low,
        ReadOnly,
        "read one service release"
    ),
    action!(
        "release.update",
        "ServiceRelease",
        Medium,
        Planned,
        "update service release metadata"
    ),
    action!(
        "release.delete",
        "ServiceRelease",
        High,
        ConfirmedPlan,
        "delete a service release record"
    ),
    action!(
        "release.validate",
        "ServiceRelease",
        Low,
        ReadOnly,
        "validate a service release manifest"
    ),
    action!(
        "release.install",
        "ServiceRelease",
        Medium,
        ConfirmedPlan,
        "install a service release to a host"
    ),
    action!(
        "release.rollback",
        "ServiceRelease",
        High,
        ConfirmedPlan,
        "rollback a service release"
    ),
    action!("host.create", "Host", Medium, Planned, "register a host"),
    action!("host.list", "Host", Low, ReadOnly, "list hosts"),
    action!("host.get", "Host", Low, ReadOnly, "read one host"),
    action!(
        "host.update",
        "Host",
        Medium,
        Planned,
        "update host metadata"
    ),
    action!("host.delete", "Host", High, ConfirmedPlan, "remove a host"),
    action!(
        "host.health.check",
        "Host",
        Low,
        Direct,
        "check host health"
    ),
    action!(
        "host.start",
        "Host",
        Medium,
        Planned,
        "start every service registered on a host"
    ),
    action!(
        "host.stop",
        "Host",
        High,
        ConfirmedPlan,
        "stop every service registered on a host"
    ),
    action!(
        "service.create",
        "Service",
        Medium,
        Planned,
        "create host service metadata"
    ),
    action!("service.list", "Service", Low, ReadOnly, "list services"),
    action!("service.get", "Service", Low, ReadOnly, "read one service"),
    action!(
        "service.update",
        "Service",
        Medium,
        Planned,
        "update service metadata"
    ),
    action!(
        "service.delete",
        "Service",
        High,
        ConfirmedPlan,
        "delete a service from a host"
    ),
    action!(
        "service.start",
        "Service",
        Medium,
        Planned,
        "start a service"
    ),
    action!(
        "service.stop",
        "Service",
        High,
        ConfirmedPlan,
        "stop a service"
    ),
    action!(
        "service.restart",
        "Service",
        High,
        ConfirmedPlan,
        "restart a service"
    ),
    action!(
        "service.enable",
        "Service",
        Medium,
        ConfirmedPlan,
        "enable a service"
    ),
    action!(
        "service.disable",
        "Service",
        High,
        ConfirmedPlan,
        "disable a service"
    ),
    action!(
        "service.health.check",
        "Service",
        Low,
        Direct,
        "check service health"
    ),
    action!(
        "endpoint.create",
        "Endpoint",
        Medium,
        Planned,
        "register an endpoint"
    ),
    action!("endpoint.list", "Endpoint", Low, ReadOnly, "list endpoints"),
    action!(
        "endpoint.get",
        "Endpoint",
        Low,
        ReadOnly,
        "read one endpoint"
    ),
    action!(
        "endpoint.update",
        "Endpoint",
        Medium,
        ConfirmedPlan,
        "update an endpoint"
    ),
    action!(
        "endpoint.delete",
        "Endpoint",
        High,
        ConfirmedPlan,
        "delete an endpoint and related links"
    ),
    action!(
        "endpoint.health.check",
        "Endpoint",
        Low,
        Direct,
        "check endpoint health"
    ),
    action!(
        "link.create",
        "Link",
        High,
        ConfirmedPlan,
        "create an endpoint link"
    ),
    action!("link.list", "Link", Low, ReadOnly, "list links"),
    action!("link.get", "Link", Low, ReadOnly, "read one link"),
    action!("link.update", "Link", High, ConfirmedPlan, "update a link"),
    action!("link.delete", "Link", High, ConfirmedPlan, "delete a link"),
    action!(
        "link.enable",
        "Link",
        Medium,
        ConfirmedPlan,
        "enable an endpoint link"
    ),
    action!(
        "link.disable",
        "Link",
        High,
        ConfirmedPlan,
        "disable an endpoint link"
    ),
    action!(
        "link.health.check",
        "Link",
        Low,
        Direct,
        "check link health"
    ),
    action!(
        "route.create",
        "Route",
        Medium,
        Planned,
        "register a gateway route"
    ),
    action!("route.list", "Route", Low, ReadOnly, "list gateway routes"),
    action!(
        "route.get",
        "Route",
        Low,
        ReadOnly,
        "read one gateway route"
    ),
    action!(
        "route.update",
        "Route",
        Medium,
        Planned,
        "update a gateway route"
    ),
    action!(
        "route.delete",
        "Route",
        High,
        ConfirmedPlan,
        "delete a gateway route"
    ),
    action!(
        "route.validate",
        "Route",
        Low,
        ReadOnly,
        "validate route declarations"
    ),
    action!(
        "route.apply",
        "Route",
        High,
        ConfirmedPlan,
        "apply routes to gateway"
    ),
    action!(
        "frontend.create",
        "FrontendEntry",
        Medium,
        Planned,
        "register a frontend entry"
    ),
    action!(
        "frontend.list",
        "FrontendEntry",
        Low,
        ReadOnly,
        "list frontend entries"
    ),
    action!(
        "frontend.get",
        "FrontendEntry",
        Low,
        ReadOnly,
        "read one frontend entry"
    ),
    action!(
        "frontend.update",
        "FrontendEntry",
        Medium,
        Planned,
        "update a frontend entry"
    ),
    action!(
        "frontend.delete",
        "FrontendEntry",
        High,
        ConfirmedPlan,
        "delete a frontend entry"
    ),
    action!(
        "frontend.validate",
        "FrontendEntry",
        Low,
        ReadOnly,
        "validate frontend declarations"
    ),
    action!(
        "frontend.publish",
        "FrontendEntry",
        High,
        ConfirmedPlan,
        "publish frontend registry to gateway"
    ),
    action!(
        "migration.create",
        "Migration",
        Medium,
        Planned,
        "register a migration"
    ),
    action!(
        "migration.list",
        "Migration",
        Low,
        ReadOnly,
        "list migrations"
    ),
    action!(
        "migration.get",
        "Migration",
        Low,
        ReadOnly,
        "read one migration"
    ),
    action!(
        "migration.update",
        "Migration",
        Medium,
        Planned,
        "update migration metadata"
    ),
    action!(
        "migration.delete",
        "Migration",
        High,
        ConfirmedPlan,
        "delete a migration record"
    ),
    action!(
        "migration.validate",
        "Migration",
        Low,
        ReadOnly,
        "validate migration metadata"
    ),
    action!(
        "migration.apply",
        "Migration",
        High,
        ConfirmedPlan,
        "apply a service migration"
    ),
    action!(
        "migration.rollback",
        "Migration",
        High,
        ConfirmedPlan,
        "rollback a service migration"
    ),
    action!(
        "permission.create",
        "Permission",
        Medium,
        Planned,
        "register a permission"
    ),
    action!(
        "permission.list",
        "Permission",
        Low,
        ReadOnly,
        "list permissions"
    ),
    action!(
        "permission.get",
        "Permission",
        Low,
        ReadOnly,
        "read one permission"
    ),
    action!(
        "permission.update",
        "Permission",
        Medium,
        Planned,
        "update a permission"
    ),
    action!(
        "permission.delete",
        "Permission",
        High,
        ConfirmedPlan,
        "delete a permission"
    ),
    action!(
        "permission.validate",
        "Permission",
        Low,
        ReadOnly,
        "validate permission declarations"
    ),
    action!(
        "permission.sync",
        "Permission",
        High,
        ConfirmedPlan,
        "sync permissions to auth-service"
    ),
    action!(
        "redis.create",
        "RedisResource",
        Medium,
        Planned,
        "register a Redis resource"
    ),
    action!(
        "redis.list",
        "RedisResource",
        Low,
        ReadOnly,
        "list Redis resources"
    ),
    action!(
        "redis.get",
        "RedisResource",
        Low,
        ReadOnly,
        "read one Redis resource"
    ),
    action!(
        "redis.update",
        "RedisResource",
        Medium,
        Planned,
        "update a Redis resource"
    ),
    action!(
        "redis.delete",
        "RedisResource",
        High,
        ConfirmedPlan,
        "delete a Redis resource"
    ),
    action!(
        "redis.validate",
        "RedisResource",
        Low,
        ReadOnly,
        "validate Redis declarations"
    ),
    action!(
        "redis.apply",
        "RedisResource",
        High,
        ConfirmedPlan,
        "apply Redis resource declarations"
    ),
    action!(
        "storage.create",
        "StorageResource",
        Medium,
        Planned,
        "register a storage resource"
    ),
    action!(
        "storage.list",
        "StorageResource",
        Low,
        ReadOnly,
        "list storage resources"
    ),
    action!(
        "storage.get",
        "StorageResource",
        Low,
        ReadOnly,
        "read one storage resource"
    ),
    action!(
        "storage.update",
        "StorageResource",
        Medium,
        Planned,
        "update a storage resource"
    ),
    action!(
        "storage.delete",
        "StorageResource",
        High,
        ConfirmedPlan,
        "delete a storage resource"
    ),
    action!(
        "storage.validate",
        "StorageResource",
        Low,
        ReadOnly,
        "validate storage declarations"
    ),
    action!(
        "storage.apply",
        "StorageResource",
        High,
        ConfirmedPlan,
        "apply storage resource declarations"
    ),
    action!(
        "config.create",
        "Config",
        Medium,
        Planned,
        "create rendered service config"
    ),
    action!("config.list", "Config", Low, ReadOnly, "list configs"),
    action!("config.get", "Config", Low, ReadOnly, "read one config"),
    action!(
        "config.update",
        "Config",
        Medium,
        Planned,
        "update config metadata"
    ),
    action!(
        "config.delete",
        "Config",
        High,
        ConfirmedPlan,
        "delete rendered config"
    ),
    action!(
        "config.render",
        "Config",
        Medium,
        Planned,
        "render config from endpoints links and secrets"
    ),
    action!(
        "config.validate",
        "Config",
        Low,
        ReadOnly,
        "validate rendered config"
    ),
    action!(
        "secret.create",
        "Secret",
        High,
        ConfirmedPlan,
        "create a secret reference"
    ),
    action!(
        "secret.list",
        "Secret",
        Low,
        ReadOnly,
        "list secret references"
    ),
    action!(
        "secret.get",
        "Secret",
        Low,
        ReadOnly,
        "read secret metadata"
    ),
    action!(
        "secret.update",
        "Secret",
        High,
        ConfirmedPlan,
        "update a secret reference"
    ),
    action!(
        "secret.delete",
        "Secret",
        High,
        ConfirmedPlan,
        "delete a secret reference"
    ),
    action!(
        "secret.distribute",
        "Secret",
        High,
        ConfirmedPlan,
        "distribute secret to a service"
    ),
    action!(
        "topology.create",
        "Topology",
        Medium,
        ConfirmedPlan,
        "create a topology snapshot"
    ),
    action!(
        "topology.list",
        "Topology",
        Low,
        ReadOnly,
        "list topology snapshots"
    ),
    action!(
        "topology.get",
        "Topology",
        Low,
        ReadOnly,
        "read current topology"
    ),
    action!(
        "topology.update",
        "Topology",
        Medium,
        ConfirmedPlan,
        "update topology snapshot metadata"
    ),
    action!(
        "topology.delete",
        "Topology",
        High,
        ConfirmedPlan,
        "delete a topology snapshot"
    ),
    action!(
        "topology.validate",
        "Topology",
        Low,
        ReadOnly,
        "validate topology"
    ),
    action!(
        "topology.apply",
        "Topology",
        High,
        ConfirmedPlan,
        "apply topology snapshot"
    ),
    action!(
        "topology.export",
        "Topology",
        Low,
        ReadOnly,
        "export topology"
    ),
    action!(
        "operation.create",
        "Operation",
        Low,
        Planned,
        "create an operation plan"
    ),
    action!(
        "operation.list",
        "Operation",
        Low,
        ReadOnly,
        "list operations"
    ),
    action!(
        "operation.get",
        "Operation",
        Low,
        ReadOnly,
        "read one operation"
    ),
    action!(
        "operation.update",
        "Operation",
        Medium,
        Planned,
        "update operation metadata"
    ),
    action!(
        "operation.delete",
        "Operation",
        High,
        ConfirmedPlan,
        "delete an operation record"
    ),
    action!(
        "operation.confirm",
        "Operation",
        Medium,
        Direct,
        "confirm an operation"
    ),
    action!(
        "operation.apply",
        "Operation",
        High,
        ConfirmedPlan,
        "apply an operation"
    ),
    action!(
        "operation.cancel",
        "Operation",
        Medium,
        Direct,
        "cancel a planned operation"
    ),
    action!(
        "operation.rollback",
        "Operation",
        High,
        ConfirmedPlan,
        "rollback an operation"
    ),
    action!("log.create", "LogView", Low, Planned, "create a log view"),
    action!("log.list", "LogView", Low, ReadOnly, "list log views"),
    action!("log.get", "LogView", Low, ReadOnly, "read one log view"),
    action!(
        "log.update",
        "LogView",
        Low,
        Planned,
        "update log view metadata"
    ),
    action!(
        "log.delete",
        "LogView",
        Medium,
        ConfirmedPlan,
        "delete a log view"
    ),
    action!(
        "log.query",
        "LogView",
        Low,
        ReadOnly,
        "query operation logs"
    ),
    action!(
        "diagnostic.create",
        "DiagnosticReport",
        Low,
        Direct,
        "run diagnostics and create a report"
    ),
    action!(
        "diagnostic.list",
        "DiagnosticReport",
        Low,
        ReadOnly,
        "list diagnostic reports"
    ),
    action!(
        "diagnostic.get",
        "DiagnosticReport",
        Low,
        ReadOnly,
        "read one diagnostic report"
    ),
    action!(
        "diagnostic.update",
        "DiagnosticReport",
        Medium,
        Planned,
        "update diagnostic metadata"
    ),
    action!(
        "diagnostic.delete",
        "DiagnosticReport",
        Medium,
        ConfirmedPlan,
        "delete a diagnostic report"
    ),
    action!(
        "diagnostic.export",
        "DiagnosticReport",
        Low,
        ReadOnly,
        "export a diagnostic report"
    ),
];

pub fn action_catalog() -> &'static [ActionDescriptor] {
    ACTION_CATALOG
}

pub fn action_descriptor(action: &str) -> Option<&'static ActionDescriptor> {
    ACTION_CATALOG
        .iter()
        .find(|descriptor| descriptor.action == action)
}

pub fn validate_action_catalog(schemas: &SharedSchemas) -> Result<Vec<ActionDescriptor>> {
    let schema_actions = schemas
        .actions
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if schema_actions.len() != schemas.actions.len() {
        return Err(OrchestratorError::Dependency(
            "Action Registry contains duplicate actions".to_string(),
        ));
    }

    let catalog_actions = ACTION_CATALOG
        .iter()
        .map(|descriptor| descriptor.action)
        .collect::<HashSet<_>>();
    if catalog_actions.len() != ACTION_CATALOG.len() {
        return Err(OrchestratorError::Dependency(
            "Core Action Catalog contains duplicate actions".to_string(),
        ));
    }
    if schema_actions != catalog_actions {
        return Err(OrchestratorError::Dependency(
            "Action Registry and Core Action Catalog differ".to_string(),
        ));
    }

    validate_layers()?;
    validate_crud_coverage()?;

    let layer_by_prefix = ACTION_LAYERS
        .iter()
        .map(|layer| (layer.prefix, layer.target_type))
        .collect::<BTreeMap<_, _>>();
    for descriptor in ACTION_CATALOG {
        let Some((prefix, target_type)) = layer_by_prefix
            .iter()
            .find(|(prefix, _)| descriptor.action.starts_with(**prefix))
        else {
            return Err(OrchestratorError::Dependency(format!(
                "action {} does not use a formal orchestration prefix",
                descriptor.action
            )));
        };
        if descriptor.target_type != *target_type {
            return Err(OrchestratorError::Dependency(format!(
                "action {} uses target {}, expected {} for prefix {}",
                descriptor.action, descriptor.target_type, target_type, prefix
            )));
        }
        if !CORE_ACTION_TARGETS.contains(&descriptor.target_type) {
            return Err(OrchestratorError::Dependency(format!(
                "action {} uses non-core target {}",
                descriptor.action, descriptor.target_type
            )));
        }
        if descriptor.risk == ActionRisk::High && !descriptor.plan_mode.requires_confirmation() {
            return Err(OrchestratorError::Dependency(format!(
                "high risk action {} must require confirmation",
                descriptor.action
            )));
        }
    }

    Ok(ACTION_CATALOG.to_vec())
}

fn validate_layers() -> Result<()> {
    let prefixes = FORMAL_ACTION_PREFIXES
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let layer_prefixes = ACTION_LAYERS
        .iter()
        .map(|layer| layer.prefix)
        .collect::<HashSet<_>>();
    if prefixes != layer_prefixes {
        return Err(OrchestratorError::Dependency(
            "formal action prefixes and action layers differ".to_string(),
        ));
    }
    let layer_targets = ACTION_LAYERS
        .iter()
        .map(|layer| layer.target_type)
        .collect::<HashSet<_>>();
    let core_targets = CORE_ACTION_TARGETS.iter().copied().collect::<HashSet<_>>();
    if layer_targets != core_targets {
        return Err(OrchestratorError::Dependency(
            "core action targets and action layers differ".to_string(),
        ));
    }
    Ok(())
}

fn validate_crud_coverage() -> Result<()> {
    let actions = ACTION_CATALOG
        .iter()
        .map(|descriptor| descriptor.action)
        .collect::<HashSet<_>>();
    for layer in ACTION_LAYERS.iter().filter(|layer| layer.requires_crud) {
        for verb in ["create", "list", "get", "update", "delete"] {
            let action = format!("{}{}", layer.prefix, verb);
            if !actions.contains(action.as_str()) {
                return Err(OrchestratorError::Dependency(format!(
                    "action layer {} is missing CRUD action {}",
                    layer.target_type, action
                )));
            }
        }
    }
    Ok(())
}
