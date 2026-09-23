#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum V1Role {
    Viewer,
    Operator,
    Admin,
}

impl V1Role {
    pub const fn permission(self) -> &'static str {
        match self {
            Self::Viewer => "orchestrator.read",
            Self::Operator => "orchestrator.operate",
            Self::Admin => "orchestrator.admin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1ActionDescriptor {
    pub action_id: &'static str,
    pub target_type: &'static str,
    pub role: V1Role,
    pub asynchronous: bool,
}

const fn action(
    action_id: &'static str,
    target_type: &'static str,
    role: V1Role,
    asynchronous: bool,
) -> V1ActionDescriptor {
    V1ActionDescriptor {
        action_id,
        target_type,
        role,
        asynchronous,
    }
}

/// The only public v1 action vocabulary. Internal release-pipeline steps such
/// as route/config/secret/migration are deliberately absent.
pub const V1_ACTIONS: &[V1ActionDescriptor] = &[
    action("catalog.list", "Catalog", V1Role::Viewer, false),
    action("catalog.search", "Catalog", V1Role::Viewer, false),
    action("catalog.register", "Catalog", V1Role::Admin, false),
    action("catalog.remove", "Catalog", V1Role::Admin, false),
    action("release.import", "Release", V1Role::Operator, false),
    action("release.validate", "Release", V1Role::Operator, false),
    action("release.install", "Release", V1Role::Operator, true),
    action("release.upgrade", "Release", V1Role::Operator, true),
    action("release.rollback", "Release", V1Role::Operator, true),
    action("release.delete", "Release", V1Role::Admin, false),
    action("node.register", "Node", V1Role::Admin, false),
    action("node.revoke", "Node", V1Role::Admin, false),
    action("node.list", "Node", V1Role::Viewer, false),
    action("node.health", "Node", V1Role::Viewer, false),
    action("node.drain", "Node", V1Role::Admin, true),
    action("node.remove", "Node", V1Role::Admin, true),
    action("deployment.list", "Deployment", V1Role::Viewer, false),
    action("deployment.get", "Deployment", V1Role::Viewer, false),
    action("deployment.start", "Deployment", V1Role::Operator, true),
    action("deployment.stop", "Deployment", V1Role::Operator, true),
    action("deployment.restart", "Deployment", V1Role::Operator, true),
    action("deployment.uninstall", "Deployment", V1Role::Admin, true),
    action("deployment.health", "Deployment", V1Role::Viewer, false),
    action("resource.purge", "ResourceClaim", V1Role::Admin, true),
    action("topology.draft", "Topology", V1Role::Operator, false),
    action("topology.revision", "Topology", V1Role::Operator, false),
    action(
        "topology.endpoint.edit",
        "Topology",
        V1Role::Operator,
        false,
    ),
    action("topology.link.edit", "Topology", V1Role::Operator, false),
    action("topology.validate", "Topology", V1Role::Operator, false),
    action("topology.diff", "Topology", V1Role::Operator, false),
    action("topology.apply", "Topology", V1Role::Operator, true),
    action("topology.rollback", "Topology", V1Role::Operator, true),
    action("topology.status", "Topology", V1Role::Viewer, false),
    action("topology.export", "Topology", V1Role::Viewer, false),
    action("operation.plan", "Operation", V1Role::Operator, false),
    action("operation.confirm", "Operation", V1Role::Operator, false),
    action("operation.apply", "Operation", V1Role::Operator, true),
    action("operation.cancel", "Operation", V1Role::Operator, true),
    action("operation.retry", "Operation", V1Role::Operator, true),
    action("operation.rollback", "Operation", V1Role::Operator, true),
    action("operation.logs", "Operation", V1Role::Viewer, false),
    action("operation.events", "Operation", V1Role::Viewer, false),
    // Report creation is a bounded transactional snapshot in v1; unlike
    // install/apply it does not perform remote side effects and returns 201.
    action("diagnostic.create", "Diagnostic", V1Role::Operator, false),
    action("diagnostic.list", "Diagnostic", V1Role::Viewer, false),
    action("diagnostic.get", "Diagnostic", V1Role::Viewer, false),
    action("diagnostic.export", "Diagnostic", V1Role::Viewer, false),
];

pub fn v1_action(action_id: &str) -> Option<&'static V1ActionDescriptor> {
    V1_ACTIONS
        .iter()
        .find(|descriptor| descriptor.action_id == action_id)
}
