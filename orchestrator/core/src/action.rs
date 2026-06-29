use crate::{OrchestratorError, Result, SharedSchemas};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionRisk {
    Low,
    Medium,
    High,
}

impl ActionRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
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
            Self::ReadOnly => "只读",
            Self::Direct => "直接执行",
            Self::Planned => "生成计划",
            Self::ConfirmedPlan => "计划并确认",
        }
    }

    pub fn plan_requirement(self) -> &'static str {
        match self {
            Self::ReadOnly => "无需",
            Self::Direct => "无需",
            Self::Planned => "必须",
            Self::ConfirmedPlan => "必须确认",
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

pub const CORE_ACTION_TARGETS: &[&str] = &[
    "Service",
    "Set",
    "Endpoint",
    "Link",
    "Operation",
    "Topology",
    "LogView",
    "DiagnosticReport",
];

pub const FORMAL_ACTION_PREFIXES: &[&str] = &[
    "deployment.",
    "service.",
    "set.",
    "endpoint.",
    "link.",
    "topology.",
    "operation.",
    "diagnostics.",
];

pub const ACTION_CATALOG: &[ActionDescriptor] = &[
    ActionDescriptor {
        action: "deployment.create",
        target_type: "Topology",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "创建新的编排拓扑入口",
    },
    ActionDescriptor {
        action: "deployment.open",
        target_type: "Topology",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "打开已有编排拓扑视图",
    },
    ActionDescriptor {
        action: "deployment.diagnose",
        target_type: "DiagnosticReport",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::Direct,
        summary: "生成部署诊断报告",
    },
    ActionDescriptor {
        action: "service.import",
        target_type: "Service",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::Planned,
        summary: "导入 Service 定义",
    },
    ActionDescriptor {
        action: "service.validate",
        target_type: "Service",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "校验 service.yaml",
    },
    ActionDescriptor {
        action: "service.install",
        target_type: "Service",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "安装 Service 并声明默认 Endpoint",
    },
    ActionDescriptor {
        action: "service.enable",
        target_type: "Service",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "启用 Service",
    },
    ActionDescriptor {
        action: "service.disable",
        target_type: "Service",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "禁用 Service",
    },
    ActionDescriptor {
        action: "service.start",
        target_type: "Service",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::Planned,
        summary: "启动 Service",
    },
    ActionDescriptor {
        action: "service.stop",
        target_type: "Service",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "停止 Service",
    },
    ActionDescriptor {
        action: "service.restart",
        target_type: "Service",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "重启 Service",
    },
    ActionDescriptor {
        action: "service.delete",
        target_type: "Service",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "删除 Service",
    },
    ActionDescriptor {
        action: "service.logs.view",
        target_type: "LogView",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "查看 Service 日志",
    },
    ActionDescriptor {
        action: "service.health.check",
        target_type: "Service",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::Direct,
        summary: "检查 Service 健康状态",
    },
    ActionDescriptor {
        action: "set.import",
        target_type: "Set",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::Planned,
        summary: "导入 Set 定义",
    },
    ActionDescriptor {
        action: "set.validate",
        target_type: "Set",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "校验 Set",
    },
    ActionDescriptor {
        action: "set.expand",
        target_type: "Set",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "展开 Set 服务和默认 Link",
    },
    ActionDescriptor {
        action: "set.apply",
        target_type: "Set",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "应用 Set 到拓扑计划",
    },
    ActionDescriptor {
        action: "set.compare",
        target_type: "Set",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "比较两个 Set",
    },
    ActionDescriptor {
        action: "endpoint.register",
        target_type: "Endpoint",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::Planned,
        summary: "注册 Endpoint",
    },
    ActionDescriptor {
        action: "endpoint.update",
        target_type: "Endpoint",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "修改 Endpoint",
    },
    ActionDescriptor {
        action: "endpoint.delete",
        target_type: "Endpoint",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "删除 Endpoint 及相关 Link",
    },
    ActionDescriptor {
        action: "endpoint.health.check",
        target_type: "Endpoint",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::Direct,
        summary: "检查 Endpoint 健康状态",
    },
    ActionDescriptor {
        action: "link.create",
        target_type: "Link",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "创建 Endpoint 到 Endpoint 的 Link",
    },
    ActionDescriptor {
        action: "link.update",
        target_type: "Link",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "修改 Link 配置",
    },
    ActionDescriptor {
        action: "link.delete",
        target_type: "Link",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "删除 Link",
    },
    ActionDescriptor {
        action: "link.health.check",
        target_type: "Link",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::Direct,
        summary: "检查 Link 健康和延迟",
    },
    ActionDescriptor {
        action: "topology.load",
        target_type: "Topology",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "读取 Topology",
    },
    ActionDescriptor {
        action: "topology.validate",
        target_type: "Topology",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "校验 Topology",
    },
    ActionDescriptor {
        action: "topology.apply",
        target_type: "Topology",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "应用 Topology 快照",
    },
    ActionDescriptor {
        action: "topology.export",
        target_type: "Topology",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "导出 Topology",
    },
    ActionDescriptor {
        action: "operation.plan",
        target_type: "Operation",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::Planned,
        summary: "生成 Operation 计划",
    },
    ActionDescriptor {
        action: "operation.confirm",
        target_type: "Operation",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::Direct,
        summary: "确认 Operation",
    },
    ActionDescriptor {
        action: "operation.apply",
        target_type: "Operation",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "执行已确认 Operation",
    },
    ActionDescriptor {
        action: "operation.cancel",
        target_type: "Operation",
        risk: ActionRisk::Medium,
        plan_mode: ActionPlanMode::Direct,
        summary: "取消未执行 Operation",
    },
    ActionDescriptor {
        action: "operation.rollback",
        target_type: "Operation",
        risk: ActionRisk::High,
        plan_mode: ActionPlanMode::ConfirmedPlan,
        summary: "回滚 Operation",
    },
    ActionDescriptor {
        action: "operation.logs.view",
        target_type: "LogView",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "查看 Operation 日志",
    },
    ActionDescriptor {
        action: "diagnostics.run",
        target_type: "DiagnosticReport",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::Direct,
        summary: "运行诊断",
    },
    ActionDescriptor {
        action: "diagnostics.export",
        target_type: "DiagnosticReport",
        risk: ActionRisk::Low,
        plan_mode: ActionPlanMode::ReadOnly,
        summary: "导出诊断报告",
    },
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
            "Action Registry 存在重复 action".to_string(),
        ));
    }

    let catalog_actions = ACTION_CATALOG
        .iter()
        .map(|descriptor| descriptor.action)
        .collect::<HashSet<_>>();
    if catalog_actions.len() != ACTION_CATALOG.len() {
        return Err(OrchestratorError::Dependency(
            "Core Action Catalog 存在重复 action".to_string(),
        ));
    }
    if schema_actions != catalog_actions {
        return Err(OrchestratorError::Dependency(
            "Action Registry 与 Core Action Catalog 不一致".to_string(),
        ));
    }

    for descriptor in ACTION_CATALOG {
        if !FORMAL_ACTION_PREFIXES
            .iter()
            .any(|prefix| descriptor.action.starts_with(*prefix))
        {
            return Err(OrchestratorError::Dependency(format!(
                "action 不属于正式编排前缀: {}",
                descriptor.action
            )));
        }
        if !CORE_ACTION_TARGETS.contains(&descriptor.target_type) {
            return Err(OrchestratorError::Dependency(format!(
                "action {} 使用了非核心对象 {}",
                descriptor.action, descriptor.target_type
            )));
        }
        if descriptor.risk == ActionRisk::High && !descriptor.plan_mode.requires_confirmation() {
            return Err(OrchestratorError::Dependency(format!(
                "高风险 action {} 必须要求确认",
                descriptor.action
            )));
        }
    }

    Ok(ACTION_CATALOG.to_vec())
}
