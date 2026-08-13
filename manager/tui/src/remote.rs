//! Remote control-plane TUI.
//!
//! This is intentionally a separate surface from the compatibility/local
//! console.  It talks only to `/api/v1`, so a remote command cannot silently
//! mutate an in-process memory store.

use crate::api_client::{
    ApiClient, ApiError, ApiSuccess, CapabilitySet, CatalogSourceInput, InstallApiBindingSelection,
    InstallTopologySelection, ResourcePurgeInput, StoreInstallInput, StorePackageQuery,
    StorePipelineOptions,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use serde_json::{Map, Value, json};
use std::fs;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const COMPOSITION_TUI_STATUS_KEY: &str = "tui_composition_status";
const COMPOSITION_TUI_NOTICE: &str = "Composition install input editing is unsupported in this TUI; use the Web manager to supply inputs and install.";
const COMPOSITION_TUI_MALFORMED: &str = "Composition status unavailable: the control-plane CompositionPlan response was malformed. Use the Web manager; this TUI will not display or install it.";

#[cfg(test)]
const TUI_V1_CONTROL_ACTIONS: &[&str] = &[
    "catalog.list",
    "catalog.search",
    "catalog.register",
    "catalog.remove",
    "release.import",
    "release.validate",
    "release.install",
    "release.upgrade",
    "release.rollback",
    "release.delete",
    "topology.draft",
    "topology.revision",
    "topology.endpoint.edit",
    "topology.link.edit",
    "topology.validate",
    "topology.diff",
    "topology.apply",
    "topology.rollback",
    "topology.status",
    "topology.export",
    "operation.plan",
    "operation.confirm",
    "operation.apply",
    "operation.cancel",
    "operation.retry",
    "operation.rollback",
    "operation.logs",
    "operation.events",
    "node.list",
    "node.health",
    "node.register",
    "node.revoke",
    "node.drain",
    "node.remove",
    "deployment.list",
    "deployment.get",
    "deployment.health",
    "deployment.start",
    "deployment.stop",
    "deployment.restart",
    "deployment.uninstall",
    "resource.purge",
    "diagnostic.create",
    "diagnostic.list",
    "diagnostic.get",
    "diagnostic.export",
];

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum RemotePage {
    Store,
    Topology,
    Operations,
    Nodes,
    Deployments,
    Diagnostics,
}

impl RemotePage {
    const ALL: [Self; 6] = [
        Self::Store,
        Self::Topology,
        Self::Operations,
        Self::Nodes,
        Self::Deployments,
        Self::Diagnostics,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Store => "Store",
            Self::Topology => "Topology",
            Self::Operations => "Operations",
            Self::Nodes => "Nodes",
            Self::Deployments => "Deployments",
            Self::Diagnostics => "Diagnostics",
        }
    }

    fn list_capability(self) -> &'static str {
        match self {
            Self::Store => "catalog.search",
            Self::Topology => "topology.export",
            Self::Operations => "operation.logs",
            Self::Nodes => "node.list",
            Self::Deployments => "deployment.list",
            Self::Diagnostics => "diagnostic.list",
        }
    }

    fn list_command(self, cursor: Option<String>) -> RemoteCommand {
        match self {
            Self::Store => RemoteCommand::StorePackages {
                query: StorePackageQuery {
                    cursor,
                    ..StorePackageQuery::default()
                },
            },
            Self::Topology => RemoteCommand::TopologyList { cursor },
            Self::Operations => RemoteCommand::OperationList { cursor },
            Self::Nodes => RemoteCommand::NodeList { cursor },
            Self::Deployments => RemoteCommand::DeploymentList { cursor },
            Self::Diagnostics => RemoteCommand::DiagnosticList { cursor },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCommand {
    Capabilities,
    CatalogList {
        cursor: Option<String>,
    },
    CatalogRegister {
        id: String,
        url: String,
        required_key_id: String,
        auth_secret_ref: Option<String>,
        public_key: Option<String>,
    },
    CatalogRemove {
        source_id: String,
    },
    StorePackages {
        query: StorePackageQuery,
    },
    StoreImport {
        service_id: String,
        target_node_id: String,
        version: Option<String>,
        catalog_source_id: Option<String>,
        channel: Option<String>,
    },
    StoreValidate {
        service_id: String,
        target_node_id: String,
        version: Option<String>,
        catalog_source_id: Option<String>,
        channel: Option<String>,
        bindings: Vec<InstallApiBindingSelection>,
        topology: Option<InstallTopologySelection>,
        pipeline_options_path: Option<String>,
    },
    StoreInstall {
        service_id: String,
        target_node_id: String,
        version: Option<String>,
        catalog_source_id: Option<String>,
        channel: Option<String>,
        bindings: Vec<InstallApiBindingSelection>,
        topology: Option<InstallTopologySelection>,
        pipeline_options_path: Option<String>,
    },
    StoreUpgrade {
        deployment_id: String,
        version: Option<String>,
        catalog_source_id: Option<String>,
        bindings: Vec<InstallApiBindingSelection>,
        topologies: Vec<InstallTopologySelection>,
    },
    StoreRollback {
        deployment_id: String,
        version: Option<String>,
        catalog_source_id: Option<String>,
        bindings: Vec<InstallApiBindingSelection>,
        topologies: Vec<InstallTopologySelection>,
    },
    StoreReleaseDelete {
        service_id: String,
        version: String,
    },
    StoreUninstall {
        deployment_id: String,
    },
    TopologyList {
        cursor: Option<String>,
    },
    TopologyGet {
        topology_id: String,
    },
    TopologyDraft {
        spec_path: String,
    },
    TopologyRevisions {
        topology_id: String,
    },
    TopologyRevisionGet {
        topology_id: String,
        revision_id: String,
    },
    TopologyRevisionCreate {
        topology_id: String,
        spec_path: String,
        if_match: String,
    },
    TopologyEndpointPut {
        topology_id: String,
        endpoint_id: String,
        spec_path: String,
        if_match: String,
    },
    TopologyEndpointDelete {
        topology_id: String,
        endpoint_id: String,
        if_match: String,
    },
    TopologyLinkPut {
        topology_id: String,
        source_endpoint: String,
        target_endpoint: String,
        spec_path: String,
        if_match: String,
    },
    TopologyLinkDelete {
        topology_id: String,
        source_endpoint: String,
        target_endpoint: String,
        if_match: String,
    },
    TopologyStatus {
        topology_id: String,
    },
    TopologyAction {
        topology_id: String,
        action: String,
        if_match: Option<String>,
        target_revision: Option<String>,
        spec_path: Option<String>,
        from_revision: Option<String>,
        to_revision: Option<String>,
    },
    OperationList {
        cursor: Option<String>,
    },
    OperationPlan {
        plan_path: String,
    },
    OperationGet {
        operation_id: String,
    },
    OperationLogs {
        operation_id: String,
    },
    OperationEvents {
        operation_id: String,
        last_event_id: Option<String>,
    },
    OperationAction {
        operation_id: String,
        action: String,
    },
    NodeList {
        cursor: Option<String>,
    },
    NodeGet {
        node_id: String,
    },
    NodeHealth {
        node_id: String,
    },
    NodeRegister {
        node_id: String,
        host_ip: String,
        role: Option<String>,
        ttl_seconds: Option<u64>,
    },
    NodeRevokeCertificates {
        node_id: String,
        reason: String,
    },
    NodeDrain {
        node_id: String,
    },
    NodeRemove {
        node_id: String,
    },
    DeploymentList {
        cursor: Option<String>,
    },
    DeploymentGet {
        deployment_id: String,
    },
    DeploymentHealth {
        deployment_id: String,
    },
    DeploymentBindings {
        deployment_id: String,
    },
    DeploymentAction {
        deployment_id: String,
        action: String,
    },
    ResourcePurge {
        claim_id: String,
        node_id: String,
        claim_digest: String,
        generation: u64,
        confirmation: String,
        reason: String,
    },
    DiagnosticList {
        cursor: Option<String>,
    },
    DiagnosticCreate,
    DiagnosticGet {
        report_id: String,
    },
    DiagnosticExport {
        report_id: String,
        format: String,
    },
}

impl RemoteCommand {
    pub fn parse(source: &str) -> Result<Self, String> {
        let tokens = split_command_line(source)?;
        let args = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        let command = match args.as_slice() {
            ["capabilities"] => Self::Capabilities,
            ["store", "catalogs"] => Self::CatalogList { cursor: None },
            ["store", "catalogs", cursor] => Self::CatalogList {
                cursor: optional_token(cursor),
            },
            ["store", "catalog", "add", id, url, required_key_id] => Self::CatalogRegister {
                id: (*id).to_string(),
                url: (*url).to_string(),
                required_key_id: (*required_key_id).to_string(),
                auth_secret_ref: None,
                public_key: None,
            },
            [
                "store",
                "catalog",
                "add",
                id,
                url,
                required_key_id,
                auth_secret_ref,
            ] => Self::CatalogRegister {
                id: (*id).to_string(),
                url: (*url).to_string(),
                required_key_id: (*required_key_id).to_string(),
                auth_secret_ref: optional_token(auth_secret_ref),
                public_key: None,
            },
            [
                "store",
                "catalog",
                "add",
                id,
                url,
                required_key_id,
                auth_secret_ref,
                public_key,
            ] => Self::CatalogRegister {
                id: (*id).to_string(),
                url: (*url).to_string(),
                required_key_id: (*required_key_id).to_string(),
                auth_secret_ref: optional_token(auth_secret_ref),
                public_key: optional_token(public_key),
            },
            ["store", "catalog", "remove", source_id] => Self::CatalogRemove {
                source_id: (*source_id).to_string(),
            },
            ["store", "list"] | ["store", "packages"] => Self::StorePackages {
                query: StorePackageQuery::default(),
            },
            ["store", "packages", cursor] => Self::StorePackages {
                query: StorePackageQuery {
                    cursor: optional_token(cursor),
                    ..StorePackageQuery::default()
                },
            },
            ["store", "search", search] => Self::StorePackages {
                query: StorePackageQuery {
                    search: optional_token(search),
                    ..StorePackageQuery::default()
                },
            },
            ["store", "search", search, channel, os, arch, variant] => Self::StorePackages {
                query: StorePackageQuery {
                    search: optional_token(search),
                    channel: optional_token(channel),
                    os: optional_token(os),
                    arch: optional_token(arch),
                    variant: optional_token(variant),
                    cursor: None,
                },
            },
            ["store", "import", service_id, target_node_id, tail @ ..]
                if tail.len() <= 3 && !service_id.contains("://") =>
            {
                Self::StoreImport {
                    service_id: (*service_id).to_string(),
                    target_node_id: (*target_node_id).to_string(),
                    version: tail.first().and_then(|value| optional_token(value)),
                    catalog_source_id: tail.get(1).and_then(|value| optional_token(value)),
                    channel: tail.get(2).and_then(|value| optional_token(value)),
                }
            }
            ["store", "validate", service_id, target_node_id, tail @ ..]
                if (4..=6).contains(&tail.len()) =>
            {
                Self::StoreValidate {
                    service_id: (*service_id).to_string(),
                    target_node_id: (*target_node_id).to_string(),
                    version: tail.first().and_then(|value| optional_token(value)),
                    catalog_source_id: tail.get(1).and_then(|value| optional_token(value)),
                    channel: tail.get(2).and_then(|value| optional_token(value)),
                    bindings: tail
                        .get(3)
                        .map(|value| parse_binding_selections(value))
                        .transpose()?
                        .unwrap_or_default(),
                    topology: tail
                        .get(4)
                        .and_then(|value| optional_token(value))
                        .map(|value| parse_topology_selection(&value))
                        .transpose()?,
                    pipeline_options_path: tail.get(5).and_then(|value| optional_token(value)),
                }
            }
            ["store", "install", service_id, target_node_id, tail @ ..]
                if (4..=6).contains(&tail.len()) =>
            {
                Self::StoreInstall {
                    service_id: (*service_id).to_string(),
                    target_node_id: (*target_node_id).to_string(),
                    version: tail.first().and_then(|value| optional_token(value)),
                    catalog_source_id: tail.get(1).and_then(|value| optional_token(value)),
                    channel: tail.get(2).and_then(|value| optional_token(value)),
                    bindings: tail
                        .get(3)
                        .map(|value| parse_binding_selections(value))
                        .transpose()?
                        .unwrap_or_default(),
                    topology: tail
                        .get(4)
                        .and_then(|value| optional_token(value))
                        .map(|value| parse_topology_selection(&value))
                        .transpose()?,
                    pipeline_options_path: tail.get(5).and_then(|value| optional_token(value)),
                }
            }
            [
                "store",
                action @ ("upgrade" | "rollback"),
                deployment_id,
                tail @ ..,
            ] if tail.len() <= 4 => {
                let deployment_id = (*deployment_id).to_string();
                let version = tail.first().and_then(|value| optional_token(value));
                let catalog_source_id = tail.get(1).and_then(|value| optional_token(value));
                let legacy_topology_position = tail
                    .get(2)
                    .is_some_and(|value| value.contains('@') && !value.contains('='));
                let bindings = if legacy_topology_position {
                    Vec::new()
                } else {
                    tail.get(2)
                        .map(|value| parse_binding_selections(value))
                        .transpose()?
                        .unwrap_or_default()
                };
                let topologies = tail
                    .get(if legacy_topology_position { 2 } else { 3 })
                    .map(|value| parse_topology_selections(value))
                    .transpose()?
                    .unwrap_or_default();
                if *action == "upgrade" {
                    Self::StoreUpgrade {
                        deployment_id,
                        version,
                        catalog_source_id,
                        bindings,
                        topologies,
                    }
                } else {
                    Self::StoreRollback {
                        deployment_id,
                        version,
                        catalog_source_id,
                        bindings,
                        topologies,
                    }
                }
            }
            ["store", "release", "delete", service_id, version] => Self::StoreReleaseDelete {
                service_id: (*service_id).to_string(),
                version: (*version).to_string(),
            },
            ["store", "uninstall", deployment_id] => Self::StoreUninstall {
                deployment_id: (*deployment_id).to_string(),
            },
            ["topology", "list"] => Self::TopologyList { cursor: None },
            ["topology", "list", cursor] => Self::TopologyList {
                cursor: optional_token(cursor),
            },
            ["topology", action @ ("get" | "export"), topology_id] => {
                let _ = action;
                Self::TopologyGet {
                    topology_id: (*topology_id).to_string(),
                }
            }
            ["topology", "export", topology_id, revision_id] => Self::TopologyRevisionGet {
                topology_id: (*topology_id).to_string(),
                revision_id: (*revision_id).to_string(),
            },
            ["topology", "draft", spec_path] => Self::TopologyDraft {
                spec_path: (*spec_path).to_string(),
            },
            ["topology", "revisions", topology_id] => Self::TopologyRevisions {
                topology_id: (*topology_id).to_string(),
            },
            ["topology", "revision", "get", topology_id, revision_id] => {
                Self::TopologyRevisionGet {
                    topology_id: (*topology_id).to_string(),
                    revision_id: (*revision_id).to_string(),
                }
            }
            [
                "topology",
                "revision",
                "create",
                topology_id,
                spec_path,
                etag,
            ] => Self::TopologyRevisionCreate {
                topology_id: (*topology_id).to_string(),
                spec_path: (*spec_path).to_string(),
                if_match: (*etag).to_string(),
            },
            [
                "topology",
                "endpoint",
                "put",
                topology_id,
                endpoint_id,
                spec_path,
                etag,
            ] => Self::TopologyEndpointPut {
                topology_id: (*topology_id).to_string(),
                endpoint_id: (*endpoint_id).to_string(),
                spec_path: (*spec_path).to_string(),
                if_match: (*etag).to_string(),
            },
            [
                "topology",
                "endpoint",
                "delete",
                topology_id,
                endpoint_id,
                etag,
            ] => Self::TopologyEndpointDelete {
                topology_id: (*topology_id).to_string(),
                endpoint_id: (*endpoint_id).to_string(),
                if_match: (*etag).to_string(),
            },
            [
                "topology",
                "link",
                "put",
                topology_id,
                source_endpoint,
                target_endpoint,
                spec_path,
                etag,
            ] => Self::TopologyLinkPut {
                topology_id: (*topology_id).to_string(),
                source_endpoint: (*source_endpoint).to_string(),
                target_endpoint: (*target_endpoint).to_string(),
                spec_path: (*spec_path).to_string(),
                if_match: (*etag).to_string(),
            },
            [
                "topology",
                "link",
                "delete",
                topology_id,
                source_endpoint,
                target_endpoint,
                etag,
            ] => Self::TopologyLinkDelete {
                topology_id: (*topology_id).to_string(),
                source_endpoint: (*source_endpoint).to_string(),
                target_endpoint: (*target_endpoint).to_string(),
                if_match: (*etag).to_string(),
            },
            ["topology", "status", topology_id] => Self::TopologyStatus {
                topology_id: (*topology_id).to_string(),
            },
            ["topology", "validate", topology_id, spec_path] => Self::TopologyAction {
                topology_id: (*topology_id).to_string(),
                action: "validate".to_string(),
                if_match: None,
                target_revision: None,
                spec_path: Some((*spec_path).to_string()),
                from_revision: None,
                to_revision: None,
            },
            ["topology", "diff", topology_id, tail @ ..] if tail.len() <= 2 => {
                Self::TopologyAction {
                    topology_id: (*topology_id).to_string(),
                    action: "diff".to_string(),
                    if_match: None,
                    target_revision: None,
                    spec_path: None,
                    from_revision: tail.first().and_then(|value| optional_token(value)),
                    to_revision: tail.get(1).and_then(|value| optional_token(value)),
                }
            }
            ["topology", "apply", topology_id, etag] => Self::TopologyAction {
                topology_id: (*topology_id).to_string(),
                action: "apply".to_string(),
                if_match: Some((*etag).to_string()),
                target_revision: None,
                spec_path: None,
                from_revision: None,
                to_revision: None,
            },
            ["topology", "rollback", topology_id, target_revision, etag] => Self::TopologyAction {
                topology_id: (*topology_id).to_string(),
                action: "rollback".to_string(),
                if_match: Some((*etag).to_string()),
                target_revision: Some((*target_revision).to_string()),
                spec_path: None,
                from_revision: None,
                to_revision: None,
            },
            ["operation", "list"] => Self::OperationList { cursor: None },
            ["operation", "list", cursor] => Self::OperationList {
                cursor: optional_token(cursor),
            },
            ["operation", "plan", plan_path] => Self::OperationPlan {
                plan_path: (*plan_path).to_string(),
            },
            ["operation", "get", operation_id] => Self::OperationGet {
                operation_id: (*operation_id).to_string(),
            },
            ["operation", "logs", operation_id] => Self::OperationLogs {
                operation_id: (*operation_id).to_string(),
            },
            ["operation", "events", operation_id] => Self::OperationEvents {
                operation_id: (*operation_id).to_string(),
                last_event_id: None,
            },
            ["operation", "events", operation_id, last_event_id] => Self::OperationEvents {
                operation_id: (*operation_id).to_string(),
                last_event_id: optional_token(last_event_id),
            },
            [
                "operation",
                action @ ("confirm" | "apply" | "cancel" | "retry" | "rollback"),
                operation_id,
            ] => Self::OperationAction {
                operation_id: (*operation_id).to_string(),
                action: (*action).to_string(),
            },
            ["node", "list"] => Self::NodeList { cursor: None },
            ["node", "list", cursor] => Self::NodeList {
                cursor: optional_token(cursor),
            },
            ["node", "get", node_id] => Self::NodeGet {
                node_id: (*node_id).to_string(),
            },
            ["node", "health", node_id] => Self::NodeHealth {
                node_id: (*node_id).to_string(),
            },
            ["node", "register", node_id, host_ip, tail @ ..] if tail.len() <= 2 => {
                Self::NodeRegister {
                    node_id: (*node_id).to_string(),
                    host_ip: (*host_ip).to_string(),
                    role: tail.first().and_then(|value| optional_token(value)),
                    ttl_seconds: tail
                        .get(1)
                        .and_then(|value| optional_token(value))
                        .map(|value| {
                            value
                                .parse::<u64>()
                                .map_err(|_| "node enrollment ttl_seconds must be an integer")
                        })
                        .transpose()?,
                }
            }
            ["node", "revoke-certificates", node_id, reason] => Self::NodeRevokeCertificates {
                node_id: (*node_id).to_string(),
                reason: (*reason).to_string(),
            },
            ["node", "drain", node_id] => Self::NodeDrain {
                node_id: (*node_id).to_string(),
            },
            ["node", "remove", node_id] => Self::NodeRemove {
                node_id: (*node_id).to_string(),
            },
            ["deployment", "list"] => Self::DeploymentList { cursor: None },
            ["deployment", "list", cursor] => Self::DeploymentList {
                cursor: optional_token(cursor),
            },
            ["deployment", "get", deployment_id] => Self::DeploymentGet {
                deployment_id: (*deployment_id).to_string(),
            },
            ["deployment", "health", deployment_id] => Self::DeploymentHealth {
                deployment_id: (*deployment_id).to_string(),
            },
            ["deployment", "bindings", deployment_id] => Self::DeploymentBindings {
                deployment_id: (*deployment_id).to_string(),
            },
            [
                "deployment",
                action @ ("start" | "stop" | "restart" | "uninstall"),
                deployment_id,
            ] => Self::DeploymentAction {
                deployment_id: (*deployment_id).to_string(),
                action: (*action).to_string(),
            },
            [
                "resource",
                "purge",
                claim_id,
                node_id,
                claim_digest,
                generation,
                confirmation,
                reason,
            ] => Self::ResourcePurge {
                claim_id: (*claim_id).to_string(),
                node_id: (*node_id).to_string(),
                claim_digest: (*claim_digest).to_string(),
                generation: generation.parse::<u64>().map_err(|_| {
                    "resource purge generation must be an integer of at least 1".to_string()
                })?,
                confirmation: (*confirmation).to_string(),
                reason: (*reason).to_string(),
            },
            ["diagnostic", "list"] => Self::DiagnosticList { cursor: None },
            ["diagnostic", "list", cursor] => Self::DiagnosticList {
                cursor: optional_token(cursor),
            },
            ["diagnostic", "create"] => Self::DiagnosticCreate,
            ["diagnostic", "get", report_id] => Self::DiagnosticGet {
                report_id: (*report_id).to_string(),
            },
            [
                "diagnostic",
                "export",
                report_id,
                format @ ("json" | "md" | "markdown"),
            ] => Self::DiagnosticExport {
                report_id: (*report_id).to_string(),
                format: (*format).to_string(),
            },
            _ => return Err(command_usage()),
        };
        Ok(command)
    }

    pub fn execute(&self, client: &ApiClient) -> Result<ApiSuccess, ApiError> {
        match self {
            Self::Capabilities => {
                let capabilities = client.capabilities(true)?;
                Ok(ApiSuccess {
                    status: 200,
                    data: json!({"actions": capabilities.actions()}),
                    meta: crate::api_client::ResponseMeta {
                        request_id: "tui-local-capability-cache".to_string(),
                        api_version: "v1".to_string(),
                        next_cursor: None,
                    },
                    etag: None,
                })
            }
            Self::CatalogList { cursor } => client.list_catalogs(cursor.as_deref()),
            Self::CatalogRegister {
                id,
                url,
                required_key_id,
                auth_secret_ref,
                public_key,
            } => {
                let mut source = CatalogSourceInput::trusted(id, url, required_key_id);
                source.auth_secret_ref = auth_secret_ref.clone().unwrap_or_default();
                source.public_key = public_key.clone();
                client.register_catalog(source)
            }
            Self::CatalogRemove { source_id } => client.remove_catalog(source_id),
            Self::StorePackages { query } => client.search_store_packages(query),
            Self::StoreImport {
                service_id,
                target_node_id,
                version,
                catalog_source_id,
                channel,
            } => client.import_release(catalog_selection_body(
                service_id,
                target_node_id,
                version.as_deref(),
                catalog_source_id.as_deref(),
                channel.as_deref(),
            )),
            Self::StoreValidate {
                service_id,
                target_node_id,
                version,
                catalog_source_id,
                channel,
                bindings,
                topology,
                pipeline_options_path,
            } => {
                let pipeline = load_store_pipeline_options(pipeline_options_path.as_deref())?;
                let body = catalog_binding_selection_body(
                    service_id,
                    target_node_id,
                    version.as_deref(),
                    catalog_source_id.as_deref(),
                    channel.as_deref(),
                    StoreBindingSelection {
                        bindings,
                        topology: topology.as_ref(),
                        pipeline: &pipeline,
                    },
                );
                let fingerprint = selection_fingerprint(&body);
                let response = client.validate_release(body.clone())?;
                let response = with_selection_fingerprint(response, &fingerprint);
                Ok(with_composition_tui_status(response, &body))
            }
            Self::StoreInstall {
                service_id,
                target_node_id,
                version,
                catalog_source_id,
                channel,
                bindings,
                topology,
                pipeline_options_path,
            } => {
                let pipeline = load_store_pipeline_options(pipeline_options_path.as_deref())?;
                let fingerprint = selection_fingerprint(&catalog_binding_selection_body(
                    service_id,
                    target_node_id,
                    version.as_deref(),
                    catalog_source_id.as_deref(),
                    channel.as_deref(),
                    StoreBindingSelection {
                        bindings,
                        topology: topology.as_ref(),
                        pipeline: &pipeline,
                    },
                ));
                let mut input = StoreInstallInput::managed(service_id, target_node_id);
                input.version.clone_from(version);
                input.catalog_source_id.clone_from(catalog_source_id);
                if let Some(channel) = channel {
                    input.channel.clone_from(channel);
                }
                input.apply_pipeline_options(&pipeline);
                input.bindings.clone_from(bindings);
                if let Some(topology) = topology {
                    input.topology_id = Some(topology.topology_id.clone());
                    input.topology_etag = topology
                        .revision_id
                        .as_ref()
                        .map(|revision| format!("\"{revision}\""));
                }
                let response = client.install_release(input)?;
                Ok(with_selection_fingerprint(response, &fingerprint))
            }
            Self::StoreUpgrade {
                deployment_id,
                version,
                catalog_source_id,
                bindings,
                topologies,
            } => {
                let (bindings, topologies) =
                    replacement_context(client, deployment_id, bindings, topologies)?;
                client.upgrade_release(replacement_selection_body(
                    deployment_id,
                    version.as_deref(),
                    catalog_source_id.as_deref(),
                    &bindings,
                    &topologies,
                ))
            }
            Self::StoreRollback {
                deployment_id,
                version,
                catalog_source_id,
                bindings,
                topologies,
            } => {
                let (bindings, topologies) =
                    replacement_context(client, deployment_id, bindings, topologies)?;
                client.rollback_release(replacement_selection_body(
                    deployment_id,
                    version.as_deref(),
                    catalog_source_id.as_deref(),
                    &bindings,
                    &topologies,
                ))
            }
            Self::StoreReleaseDelete {
                service_id,
                version,
            } => client.delete_release(service_id, version),
            Self::StoreUninstall { deployment_id } => {
                client.mutate_deployment(deployment_id, "uninstall")
            }
            Self::TopologyList { cursor } => client.list_topologies(cursor.as_deref()),
            Self::TopologyGet { topology_id } => client.topology(topology_id),
            Self::TopologyDraft { spec_path } => {
                client.create_topology_draft(load_json_document(spec_path)?)
            }
            Self::TopologyRevisions { topology_id } => client.topology_revisions(topology_id),
            Self::TopologyRevisionGet {
                topology_id,
                revision_id,
            } => client.topology_revision(topology_id, revision_id),
            Self::TopologyRevisionCreate {
                topology_id,
                spec_path,
                if_match,
            } => client.create_topology_revision(
                topology_id,
                load_json_document(spec_path)?,
                if_match,
            ),
            Self::TopologyEndpointPut {
                topology_id,
                endpoint_id,
                spec_path,
                if_match,
            } => client.put_topology_draft_endpoint(
                topology_id,
                endpoint_id,
                load_json_document(spec_path)?,
                if_match,
            ),
            Self::TopologyEndpointDelete {
                topology_id,
                endpoint_id,
                if_match,
            } => client.delete_topology_draft_endpoint(topology_id, endpoint_id, if_match),
            Self::TopologyLinkPut {
                topology_id,
                source_endpoint,
                target_endpoint,
                spec_path,
                if_match,
            } => client.put_topology_draft_link(
                topology_id,
                source_endpoint,
                target_endpoint,
                load_json_document(spec_path)?,
                if_match,
            ),
            Self::TopologyLinkDelete {
                topology_id,
                source_endpoint,
                target_endpoint,
                if_match,
            } => client.delete_topology_draft_link(
                topology_id,
                source_endpoint,
                target_endpoint,
                if_match,
            ),
            Self::TopologyStatus { topology_id } => client.topology_status(topology_id),
            Self::TopologyAction {
                topology_id,
                action,
                if_match,
                target_revision,
                spec_path,
                from_revision,
                to_revision,
            } => client.topology_action(
                topology_id,
                action,
                match action.as_str() {
                    "validate" => load_json_document(spec_path.as_deref().ok_or_else(|| {
                        ApiError::InvalidRequest(
                            "topology validate requires a TopologySpec JSON file".to_string(),
                        )
                    })?)?,
                    "diff" => json!({
                        "from_revision_id": from_revision,
                        "to_revision_id": to_revision,
                    }),
                    "rollback" => json!({
                        "revision_id": target_revision,
                    }),
                    _ => json!({}),
                },
                if_match.as_deref(),
            ),
            Self::OperationList { cursor } => client.list_operations(cursor.as_deref()),
            Self::OperationPlan { plan_path } => {
                client.plan_operation(load_json_document(plan_path)?)
            }
            Self::OperationGet { operation_id } => client.operation(operation_id),
            Self::OperationLogs { operation_id } => client.operation_logs(operation_id),
            Self::OperationEvents {
                operation_id,
                last_event_id,
            } => client.operation_events(operation_id, last_event_id.as_deref()),
            Self::OperationAction {
                operation_id,
                action,
            } => client.mutate_operation(operation_id, action),
            Self::NodeList { cursor } => client.list_nodes(cursor.as_deref()),
            Self::NodeGet { node_id } => client.node(node_id),
            Self::NodeHealth { node_id } => client.node_health(node_id),
            Self::NodeRegister {
                node_id,
                host_ip,
                role,
                ttl_seconds,
            } => client.create_node_enrollment_code(json!({
                "node_id": node_id,
                "host_ip": host_ip,
                "role": role.as_deref().unwrap_or("standalone"),
                "ttl_seconds": ttl_seconds.unwrap_or(600),
            })),
            Self::NodeRevokeCertificates { node_id, reason } => {
                client.revoke_node_certificates(node_id, reason)
            }
            Self::NodeDrain { node_id } => client.drain_node(node_id),
            Self::NodeRemove { node_id } => client.remove_node(node_id),
            Self::DeploymentList { cursor } => client.list_deployments(cursor.as_deref()),
            Self::DeploymentGet { deployment_id } => client.deployment(deployment_id),
            Self::DeploymentHealth { deployment_id } => client.deployment_health(deployment_id),
            Self::DeploymentBindings { deployment_id } => client.deployment_bindings(deployment_id),
            Self::DeploymentAction {
                deployment_id,
                action,
            } => client.mutate_deployment(deployment_id, action),
            Self::ResourcePurge {
                claim_id,
                node_id,
                claim_digest,
                generation,
                confirmation,
                reason,
            } => client.purge_resource_claim(
                claim_id,
                ResourcePurgeInput {
                    node_id: node_id.clone(),
                    claim_digest: claim_digest.clone(),
                    generation: *generation,
                    confirmation: confirmation.clone(),
                    reason: reason.clone(),
                },
            ),
            Self::DiagnosticList { cursor } => client.list_diagnostics(cursor.as_deref()),
            Self::DiagnosticCreate => client.create_diagnostic(json!({})),
            Self::DiagnosticGet { report_id } => client.diagnostic(report_id),
            Self::DiagnosticExport { report_id, format } => {
                client.export_diagnostic(report_id, format)
            }
        }
    }

    fn collection_page(&self) -> Option<RemotePage> {
        match self {
            Self::StorePackages { .. } | Self::CatalogList { .. } => Some(RemotePage::Store),
            Self::TopologyList { .. } | Self::TopologyRevisions { .. } => {
                Some(RemotePage::Topology)
            }
            Self::OperationList { .. } => Some(RemotePage::Operations),
            Self::NodeList { .. } => Some(RemotePage::Nodes),
            Self::DeploymentList { .. } => Some(RemotePage::Deployments),
            Self::DiagnosticList { .. } => Some(RemotePage::Diagnostics),
            _ => None,
        }
    }

    fn capability(&self) -> Option<String> {
        match self {
            Self::CatalogList { .. } => Some("catalog.list".to_string()),
            Self::CatalogRegister { .. } => Some("catalog.register".to_string()),
            Self::CatalogRemove { .. } => Some("catalog.remove".to_string()),
            Self::StorePackages { .. } => Some("catalog.search".to_string()),
            Self::StoreImport { .. } => Some("release.import".to_string()),
            Self::StoreValidate { .. } => Some("release.validate".to_string()),
            Self::StoreInstall { .. } => Some("release.install".to_string()),
            Self::StoreUpgrade { .. } => Some("release.upgrade".to_string()),
            Self::StoreRollback { .. } => Some("release.rollback".to_string()),
            Self::StoreReleaseDelete { .. } => Some("release.delete".to_string()),
            Self::StoreUninstall { .. } => Some("deployment.uninstall".to_string()),
            Self::TopologyList { .. }
            | Self::TopologyGet { .. }
            | Self::TopologyRevisions { .. }
            | Self::TopologyRevisionGet { .. } => Some("topology.export".to_string()),
            Self::TopologyDraft { .. } => Some("topology.draft".to_string()),
            Self::TopologyRevisionCreate { .. } => Some("topology.revision".to_string()),
            Self::TopologyEndpointPut { .. } | Self::TopologyEndpointDelete { .. } => {
                Some("topology.endpoint.edit".to_string())
            }
            Self::TopologyLinkPut { .. } | Self::TopologyLinkDelete { .. } => {
                Some("topology.link.edit".to_string())
            }
            Self::TopologyStatus { .. } => Some("topology.status".to_string()),
            Self::TopologyAction { action, .. } => Some(format!("topology.{action}")),
            Self::OperationList { .. } | Self::OperationGet { .. } | Self::OperationLogs { .. } => {
                Some("operation.logs".to_string())
            }
            Self::OperationEvents { .. } => Some("operation.events".to_string()),
            Self::OperationPlan { .. } => Some("operation.plan".to_string()),
            Self::OperationAction { action, .. } => Some(format!("operation.{action}")),
            Self::NodeList { .. } => Some("node.list".to_string()),
            Self::NodeGet { .. } | Self::NodeHealth { .. } => Some("node.health".to_string()),
            Self::NodeRegister { .. } => Some("node.register".to_string()),
            Self::NodeRevokeCertificates { .. } => Some("node.revoke".to_string()),
            Self::NodeDrain { .. } => Some("node.drain".to_string()),
            Self::NodeRemove { .. } => Some("node.remove".to_string()),
            Self::DeploymentList { .. } => Some("deployment.list".to_string()),
            Self::DeploymentGet { .. } | Self::DeploymentBindings { .. } => {
                Some("deployment.get".to_string())
            }
            Self::DeploymentHealth { .. } => Some("deployment.health".to_string()),
            Self::DeploymentAction { action, .. } => Some(format!("deployment.{action}")),
            Self::ResourcePurge { .. } => Some("resource.purge".to_string()),
            Self::DiagnosticList { .. } => Some("diagnostic.list".to_string()),
            Self::DiagnosticCreate => Some("diagnostic.create".to_string()),
            Self::DiagnosticGet { .. } => Some("diagnostic.get".to_string()),
            Self::DiagnosticExport { .. } => Some("diagnostic.export".to_string()),
            Self::Capabilities => None,
        }
    }
}

#[derive(Debug, Clone)]
struct RemoteRow {
    id: String,
    status: String,
    summary: String,
    etag: Option<String>,
    raw: Value,
}

impl RemoteRow {
    fn from_value(page: RemotePage, value: Value) -> Self {
        let keys = match page {
            RemotePage::Store => ["service_id", "module_id", "package_id", "id"],
            RemotePage::Topology => ["topology_id", "id", "revision_id", "name"],
            RemotePage::Operations => ["operation_id", "id", "action", "target"],
            RemotePage::Nodes => ["node_id", "id", "host_ip", "name"],
            RemotePage::Deployments => ["deployment_id", "id", "service_id", "name"],
            RemotePage::Diagnostics => ["report_id", "id", "operation_id", "name"],
        };
        let id = first_string(&value, &keys)
            .or_else(|| nested_string(&value, "/instance/deployment_id"))
            .unwrap_or_else(|| "<unknown>".to_string());
        let status = first_string(&value, &["status", "health", "observed_state", "lifecycle"])
            .or_else(|| nested_string(&value, "/instance/observed_state"))
            .unwrap_or_default();
        let summary = first_string(
            &value,
            &["summary", "version", "action", "host_ip", "description"],
        )
        .or_else(|| nested_string(&value, "/instance/service_id"))
        .unwrap_or_default();
        let etag = first_string(
            &value,
            &["etag", "revision_etag", "revision_id", "draft_revision_id"],
        );
        Self {
            id,
            status,
            summary,
            etag,
            raw: value,
        }
    }
}

struct WorkerEvent {
    command: RemoteCommand,
    result: Result<ApiSuccess, ApiError>,
}

struct RemoteWorker {
    sender: mpsc::Sender<Option<RemoteCommand>>,
    receiver: mpsc::Receiver<WorkerEvent>,
    pending: bool,
    join: Option<thread::JoinHandle<()>>,
}

impl RemoteWorker {
    fn spawn(client: ApiClient) -> Self {
        let (task_sender, task_receiver) = mpsc::channel::<Option<RemoteCommand>>();
        let (event_sender, event_receiver) = mpsc::channel();
        let join = thread::spawn(move || {
            while let Ok(Some(command)) = task_receiver.recv() {
                let result = command.execute(&client);
                if event_sender.send(WorkerEvent { command, result }).is_err() {
                    break;
                }
            }
        });
        Self {
            sender: task_sender,
            receiver: event_receiver,
            pending: false,
            join: Some(join),
        }
    }

    fn submit(&mut self, command: RemoteCommand) -> Result<(), String> {
        if self.pending {
            return Err("another control-plane request is still running".to_string());
        }
        self.sender
            .send(Some(command))
            .map_err(|_| "remote API worker has stopped".to_string())?;
        self.pending = true;
        Ok(())
    }

    fn try_next(&mut self) -> Option<WorkerEvent> {
        let event = self.receiver.try_recv().ok()?;
        self.pending = false;
        Some(event)
    }
}

impl Drop for RemoteWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(None);
        if !self.pending
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

struct RemoteApp {
    page: RemotePage,
    capabilities: CapabilitySet,
    worker: RemoteWorker,
    rows: Vec<RemoteRow>,
    selected: usize,
    detail: String,
    message: String,
    command_input: Option<String>,
    next_cursor: Option<String>,
}

impl RemoteApp {
    fn new(client: ApiClient) -> Result<Self> {
        let capabilities = client.capabilities(false)?;
        let page = RemotePage::ALL
            .iter()
            .copied()
            .find(|page| capabilities.supports(page.list_capability()))
            .ok_or_else(|| {
                anyhow::anyhow!("control plane publishes no readable v1 resource capability")
            })?;
        let worker = RemoteWorker::spawn(client);
        let mut app = Self {
            page,
            capabilities,
            worker,
            rows: Vec::new(),
            selected: 0,
            detail: String::new(),
            message: String::new(),
            command_input: None,
            next_cursor: None,
        };
        app.refresh();
        Ok(app)
    }

    fn refresh(&mut self) {
        if self.worker.pending {
            self.message = "request is still in progress".to_string();
            return;
        }
        self.next_cursor = None;
        self.submit(self.page.list_command(None));
    }

    fn next_result_page(&mut self) {
        let Some(cursor) = self.next_cursor.clone() else {
            self.message = "no next page".to_string();
            return;
        };
        self.submit(self.page.list_command(Some(cursor)));
    }

    fn submit(&mut self, command: RemoteCommand) {
        if let Some(capability) = command.capability()
            && !self.capabilities.supports(&capability)
        {
            self.message = format!("unavailable: control plane does not publish {capability}");
            return;
        }
        match self.worker.submit(command) {
            Ok(()) => self.message = "request in progress…".to_string(),
            Err(err) => self.message = err,
        }
    }

    fn tick(&mut self) {
        while let Some(event) = self.worker.try_next() {
            match event.result {
                Ok(response) => {
                    if event.command.collection_page() == Some(self.page) {
                        self.rows = rows_from_response(self.page, &response.data);
                        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
                        self.next_cursor.clone_from(&response.meta.next_cursor);
                    }
                    self.detail = remote_response_detail(&event.command, &response.data);
                    self.message = format!(
                        "HTTP {} · request {}{}",
                        response.status,
                        response.meta.request_id,
                        response
                            .meta
                            .next_cursor
                            .as_deref()
                            .map(|cursor| format!(" · next_cursor={cursor}"))
                            .unwrap_or_default()
                    );
                    if matches!(event.command, RemoteCommand::Capabilities)
                        && let Ok(actions) =
                            serde_json::from_value::<Vec<String>>(response.data["actions"].clone())
                    {
                        self.message = format!("{} published capabilities", actions.len());
                    }
                    if matches!(event.command, RemoteCommand::StoreValidate { .. })
                        && response.data.get(COMPOSITION_TUI_STATUS_KEY).is_some()
                    {
                        self.message.push_str(
                            " · Composition inputs are read-only here; use Web to edit/install",
                        );
                    }
                }
                Err(error) => {
                    self.detail = format!("{error:#?}");
                    self.message = match error.retry_after() {
                        Some(retry_after) => format!("{error}; retry after {retry_after}s"),
                        None => error.to_string(),
                    };
                }
            }
        }
    }

    fn next_page(&mut self) {
        if self.worker.pending {
            self.message = "wait for the current request before changing page".to_string();
            return;
        }
        let pages = RemotePage::ALL
            .iter()
            .copied()
            .filter(|page| self.capabilities.supports(page.list_capability()))
            .collect::<Vec<_>>();
        let index = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        self.page = pages[(index + 1) % pages.len()];
        self.rows.clear();
        self.selected = 0;
        self.detail.clear();
        self.next_cursor = None;
        self.refresh();
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected =
            (self.selected as isize + delta).rem_euclid(self.rows.len() as isize) as usize;
        if let Some(row) = self.rows.get(self.selected) {
            self.detail = serde_json::to_string_pretty(&row.raw).unwrap_or_default();
        }
    }

    fn selected(&self) -> Option<&RemoteRow> {
        self.rows.get(self.selected)
    }

    fn quick_action(&mut self, key: KeyCode) {
        let Some(row) = self.selected().cloned() else {
            self.message = "select a resource first".to_string();
            return;
        };
        let command = match (self.page, key) {
            (RemotePage::Store, KeyCode::Char('i')) => {
                self.command_input = Some(format!("store install {} ", row.id));
                return;
            }
            (RemotePage::Topology, KeyCode::Char('v')) => {
                self.command_input = Some(format!("topology validate {} ", row.id));
                return;
            }
            (RemotePage::Topology, KeyCode::Char('f')) => RemoteCommand::TopologyAction {
                topology_id: row.id,
                action: "diff".to_string(),
                if_match: None,
                target_revision: None,
                spec_path: None,
                from_revision: None,
                to_revision: None,
            },
            (RemotePage::Topology, KeyCode::Char('a')) => RemoteCommand::TopologyAction {
                topology_id: row.id,
                action: "apply".to_string(),
                if_match: row.etag,
                target_revision: None,
                spec_path: None,
                from_revision: None,
                to_revision: None,
            },
            (RemotePage::Topology, KeyCode::Char('h')) => RemoteCommand::TopologyStatus {
                topology_id: row.id,
            },
            (RemotePage::Topology, KeyCode::Char('R')) => RemoteCommand::TopologyRevisions {
                topology_id: row.id,
            },
            (RemotePage::Operations, KeyCode::Char('o')) => RemoteCommand::OperationLogs {
                operation_id: row.id,
            },
            (RemotePage::Operations, KeyCode::Char('e')) => RemoteCommand::OperationEvents {
                operation_id: row.id,
                last_event_id: None,
            },
            (RemotePage::Operations, KeyCode::Char('c')) => RemoteCommand::OperationAction {
                operation_id: row.id,
                action: "cancel".to_string(),
            },
            (RemotePage::Operations, KeyCode::Char('y')) => RemoteCommand::OperationAction {
                operation_id: row.id,
                action: "retry".to_string(),
            },
            (RemotePage::Operations, KeyCode::Char('b')) => RemoteCommand::OperationAction {
                operation_id: row.id,
                action: "rollback".to_string(),
            },
            (RemotePage::Nodes, KeyCode::Char('d')) => RemoteCommand::NodeDrain { node_id: row.id },
            (RemotePage::Nodes, KeyCode::Char('h')) => {
                RemoteCommand::NodeHealth { node_id: row.id }
            }
            (RemotePage::Nodes, KeyCode::Delete) => RemoteCommand::NodeRemove { node_id: row.id },
            (RemotePage::Deployments, KeyCode::F(5)) => RemoteCommand::DeploymentAction {
                deployment_id: row.id,
                action: "start".to_string(),
            },
            (RemotePage::Deployments, KeyCode::F(6)) => RemoteCommand::DeploymentAction {
                deployment_id: row.id,
                action: "stop".to_string(),
            },
            (RemotePage::Deployments, KeyCode::F(7)) => RemoteCommand::DeploymentAction {
                deployment_id: row.id,
                action: "restart".to_string(),
            },
            (RemotePage::Deployments, KeyCode::Delete) => RemoteCommand::DeploymentAction {
                deployment_id: row.id,
                action: "uninstall".to_string(),
            },
            (RemotePage::Deployments, KeyCode::Char('h')) => RemoteCommand::DeploymentHealth {
                deployment_id: row.id,
            },
            (RemotePage::Deployments, KeyCode::Char('b')) => RemoteCommand::DeploymentBindings {
                deployment_id: row.id,
            },
            (RemotePage::Diagnostics, KeyCode::Char('g')) => {
                RemoteCommand::DiagnosticGet { report_id: row.id }
            }
            (RemotePage::Diagnostics, KeyCode::Char('j')) => RemoteCommand::DiagnosticExport {
                report_id: row.id,
                format: "json".to_string(),
            },
            (RemotePage::Diagnostics, KeyCode::Char('m')) => RemoteCommand::DiagnosticExport {
                report_id: row.id,
                format: "markdown".to_string(),
            },
            _ => return,
        };
        self.submit(command);
    }

    fn handle_command_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc => self.command_input = None,
            KeyCode::Enter => {
                let source = self.command_input.take().unwrap_or_default();
                match RemoteCommand::parse(&source) {
                    Ok(command) => self.submit(command),
                    Err(error) => self.message = error,
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.command_input.as_mut() {
                    input.pop();
                }
            }
            KeyCode::Char(character) => {
                if let Some(input) = self.command_input.as_mut() {
                    input.push(character);
                }
            }
            _ => {}
        }
    }
}

pub fn execute_once(client: &ApiClient, source: &str) -> Result<Value> {
    let command = RemoteCommand::parse(source).map_err(anyhow::Error::msg)?;
    let result = command.execute(client)?;
    Ok(json!({
        "data": result.data,
        "meta": result.meta,
        "status": result.status,
        "etag": result.etag,
    }))
}

pub fn run_remote(client: ApiClient) -> Result<()> {
    let mut app = RemoteApp::new(client)?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = (|| -> Result<()> {
        loop {
            app.tick();
            terminal.draw(|frame| draw_remote(frame, &app))?;
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.command_input.is_some() {
                    app.handle_command_key(key.code);
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Tab => app.next_page(),
                    KeyCode::Char('r') => app.refresh(),
                    KeyCode::Char('n') => app.next_result_page(),
                    KeyCode::Char(':') => app.command_input = Some(String::new()),
                    KeyCode::Up => app.move_selection(-1),
                    KeyCode::Down => app.move_selection(1),
                    other => app.quick_action(other),
                }
            }
        }
    })();

    let cleanup = (|| -> Result<()> {
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;
        Ok(())
    })();
    match result {
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
        Ok(()) => cleanup,
    }
}

fn draw_remote(frame: &mut ratatui::Frame<'_>, app: &RemoteApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let pages = RemotePage::ALL
        .iter()
        .copied()
        .filter(|page| app.capabilities.supports(page.list_capability()))
        .collect::<Vec<_>>();
    let titles = pages
        .iter()
        .map(|page| Line::from(page.title()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "OJOS Orchestrator v1 remote TUI",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                " · {} capabilities · {}",
                app.capabilities.actions().len(),
                if app.worker.pending { "busy" } else { "ready" }
            )),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );
    frame.render_widget(
        Tabs::new(titles)
            .select(pages.iter().position(|page| *page == app.page).unwrap_or(0))
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(Style::default().fg(Color::Yellow)),
        chunks[1],
    );
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(chunks[2]);
    let rows = app
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let line = format!("{}  {:<16}  {}", row.id, row.status, row.summary);
            let style = if index == app.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(rows).block(Block::default().borders(Borders::ALL).title(format!(
            "{} resources ({})",
            app.page.title(),
            app.rows.len()
        ))),
        body[0],
    );
    frame.render_widget(
        Paragraph::new(app.detail.clone())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("API result / selected resource"),
            ),
        body[1],
    );
    let footer = match &app.command_input {
        Some(input) => format!(":{input}_  · Enter execute · Esc cancel"),
        None => format!(
            "{}\n{}",
            page_help(app.page, &app.capabilities),
            app.message
        ),
    };
    frame.render_widget(
        Paragraph::new(footer)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL)),
        chunks[3],
    );
}

fn rows_from_response(page: RemotePage, data: &Value) -> Vec<RemoteRow> {
    let collection_names = match page {
        RemotePage::Store => ["packages", "items", "releases"],
        RemotePage::Topology => ["topologies", "items", "revisions"],
        RemotePage::Operations => ["operations", "items", "results"],
        RemotePage::Nodes => ["nodes", "items", "results"],
        RemotePage::Deployments => ["deployments", "items", "instances"],
        RemotePage::Diagnostics => ["diagnostics", "items", "reports"],
    };
    let values = data
        .as_array()
        .or_else(|| {
            collection_names
                .iter()
                .find_map(|name| data.get(name).and_then(Value::as_array))
        })
        .cloned()
        .unwrap_or_default();
    values
        .into_iter()
        .map(|value| RemoteRow::from_value(page, value))
        .collect()
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|field| match field {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
    })
}

fn nested_string(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer).and_then(|field| match field {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn page_help(page: RemotePage, capabilities: &CapabilitySet) -> String {
    let mut keys = vec![
        "Tab page",
        "r refresh",
        "n next",
        ": command",
        "↑/↓ select",
        "q quit",
    ];
    let additions = match page {
        RemotePage::Store => [("release.install", "i install")].as_slice(),
        RemotePage::Topology => [
            ("topology.validate", "v validate"),
            ("topology.diff", "f diff"),
            ("topology.apply", "a apply"),
            ("topology.status", "h status"),
            ("topology.export", "R revisions"),
        ]
        .as_slice(),
        RemotePage::Operations => [
            ("operation.logs", "o logs"),
            ("operation.events", "e events"),
            ("operation.cancel", "c cancel"),
            ("operation.retry", "y retry"),
            ("operation.rollback", "b rollback"),
        ]
        .as_slice(),
        RemotePage::Nodes => [
            ("node.health", "h health"),
            ("node.drain", "d drain"),
            ("node.remove", "Delete remove"),
        ]
        .as_slice(),
        RemotePage::Deployments => [
            ("deployment.health", "h health"),
            ("deployment.get", "b bindings"),
            ("deployment.start", "F5 start"),
            ("deployment.stop", "F6 stop"),
            ("deployment.restart", "F7 restart"),
            ("deployment.uninstall", "Delete uninstall"),
        ]
        .as_slice(),
        RemotePage::Diagnostics => [
            ("diagnostic.get", "g get"),
            ("diagnostic.export", "j json / m markdown"),
        ]
        .as_slice(),
    };
    keys.extend(
        additions
            .iter()
            .filter(|(capability, _)| capabilities.supports(capability))
            .map(|(_, label)| *label),
    );
    keys.join(" · ")
}

fn optional_token(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "-").then(|| value.to_string())
}

fn parse_binding_selections(source: &str) -> Result<Vec<InstallApiBindingSelection>, String> {
    if source.trim().is_empty() || source.trim() == "-" {
        return Ok(Vec::new());
    }
    let mut selections = source
        .split(',')
        .map(|entry| {
            let (name, deployment) = entry.split_once('=').ok_or_else(|| {
                "binding selections must use requirement=provider-deployment[,..]".to_string()
            })?;
            let name = name.trim();
            let deployment = deployment.trim();
            if name.is_empty()
                || deployment.is_empty()
                || name.chars().any(char::is_whitespace)
                || deployment.chars().any(char::is_whitespace)
            {
                return Err(
                    "binding requirement and provider deployment must be non-empty tokens"
                        .to_string(),
                );
            }
            Ok(InstallApiBindingSelection {
                name: name.to_string(),
                provider_deployment_id: deployment.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    selections.sort_by(|left, right| left.name.cmp(&right.name));
    if selections
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        return Err("a binding requirement may be selected only once".to_string());
    }
    Ok(selections)
}

fn parse_topology_selection(source: &str) -> Result<InstallTopologySelection, String> {
    let (topology_id, revision_id) = source
        .split_once('@')
        .ok_or_else(|| "topology selection must use topology-id@applied-revision-id".to_string())?;
    let topology_id = topology_id.trim();
    let revision_id = revision_id.trim();
    if topology_id.is_empty() || revision_id.is_empty() {
        return Err("topology selection must use topology-id@applied-revision-id".to_string());
    }
    Ok(InstallTopologySelection {
        topology_id: topology_id.to_string(),
        revision_id: Some(revision_id.to_string()),
    })
}

fn parse_topology_selections(source: &str) -> Result<Vec<InstallTopologySelection>, String> {
    let mut selections = source
        .split(',')
        .map(parse_topology_selection)
        .collect::<Result<Vec<_>, _>>()?;
    selections.sort_by(|left, right| left.topology_id.cmp(&right.topology_id));
    if selections.is_empty()
        || selections
            .windows(2)
            .any(|pair| pair[0].topology_id == pair[1].topology_id)
    {
        return Err("replacement topology CAS entries must be non-empty and unique".to_string());
    }
    Ok(selections)
}

fn load_json_document(path: &str) -> Result<Value, ApiError> {
    const MAX_JSON_DOCUMENT_BYTES: u64 = 1024 * 1024;
    let metadata = fs::metadata(path).map_err(|error| {
        ApiError::InvalidRequest(format!("cannot inspect JSON document {path:?}: {error}"))
    })?;
    if !metadata.is_file() || metadata.len() > MAX_JSON_DOCUMENT_BYTES {
        return Err(ApiError::InvalidRequest(format!(
            "JSON document {path:?} must be a regular file no larger than {MAX_JSON_DOCUMENT_BYTES} bytes"
        )));
    }
    let source = fs::read_to_string(path).map_err(|error| {
        ApiError::InvalidRequest(format!("cannot read JSON document {path:?}: {error}"))
    })?;
    serde_json::from_str(&source).map_err(|error| {
        ApiError::InvalidRequest(format!("JSON document {path:?} is invalid: {error}"))
    })
}

fn catalog_selection_body(
    service_id: &str,
    target_node_id: &str,
    version: Option<&str>,
    catalog_source_id: Option<&str>,
    channel: Option<&str>,
) -> Value {
    json!({
        "service_id": service_id,
        "target_node_id": target_node_id,
        "version": version.unwrap_or_default(),
        "catalog_source_id": catalog_source_id.unwrap_or_default(),
        "channel": channel.unwrap_or("stable"),
    })
}

struct StoreBindingSelection<'a> {
    bindings: &'a [InstallApiBindingSelection],
    topology: Option<&'a InstallTopologySelection>,
    pipeline: &'a StorePipelineOptions,
}

fn catalog_binding_selection_body(
    service_id: &str,
    target_node_id: &str,
    version: Option<&str>,
    catalog_source_id: Option<&str>,
    channel: Option<&str>,
    selection: StoreBindingSelection<'_>,
) -> Value {
    let mut body = catalog_selection_body(
        service_id,
        target_node_id,
        version,
        catalog_source_id,
        channel,
    );
    body["bindings"] = json!(selection.bindings);
    body["start"] = json!(selection.pipeline.start);
    body["migration_policy"] = json!(selection.pipeline.migration_policy);
    body["config"] = selection.pipeline.config.clone();
    body["secret_refs"] = json!(selection.pipeline.secret_refs);
    if let Some(gateway_node_id) = &selection.pipeline.gateway_node_id {
        body["gateway_node_id"] = json!(gateway_node_id);
    }
    if let Some(topology) = selection.topology {
        body["topology_id"] = json!(topology.topology_id);
        body["topology_etag"] = json!(
            topology
                .revision_id
                .as_ref()
                .map(|revision| format!("\"{revision}\""))
                .unwrap_or_default()
        );
    }
    body
}

fn load_store_pipeline_options(path: Option<&str>) -> Result<StorePipelineOptions, ApiError> {
    let options = match path {
        Some(path) => serde_json::from_value(load_json_document(path)?).map_err(|error| {
            ApiError::InvalidRequest(format!(
                "Store pipeline options document {path:?} is invalid: {error}"
            ))
        })?,
        None => StorePipelineOptions::default(),
    };
    normalize_store_pipeline_options(options)
}

fn normalize_store_pipeline_options(
    mut options: StorePipelineOptions,
) -> Result<StorePipelineOptions, ApiError> {
    options.migration_policy = options.migration_policy.trim().to_ascii_uppercase();
    if !matches!(options.migration_policy.as_str(), "APPLY" | "DRY_RUN") {
        return Err(ApiError::InvalidRequest(
            "Store pipeline migration_policy must be APPLY or DRY_RUN".to_string(),
        ));
    }
    if !options.config.is_object() {
        return Err(ApiError::InvalidRequest(
            "Store pipeline config must be a JSON object".to_string(),
        ));
    }
    options.gateway_node_id = options
        .gateway_node_id
        .take()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if options
        .secret_refs
        .iter()
        .any(|(name, reference)| name.trim().is_empty() || reference.trim().is_empty())
    {
        return Err(ApiError::InvalidRequest(
            "Store pipeline secret_refs keys and references must be non-empty".to_string(),
        ));
    }
    options.secret_refs = options
        .secret_refs
        .into_iter()
        .map(|(name, reference)| (name.trim().to_string(), reference.trim().to_string()))
        .collect();
    Ok(options)
}

fn selection_fingerprint(selection: &Value) -> String {
    let encoded = serde_json::to_vec(selection).expect("selection JSON is serializable");
    let hash = encoded
        .into_iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    format!("fnv1a64:{hash:016x}")
}

fn with_selection_fingerprint(mut response: ApiSuccess, fingerprint: &str) -> ApiSuccess {
    if let Some(data) = response.data.as_object_mut() {
        data.insert(
            "selection_fingerprint".to_string(),
            Value::String(fingerprint.to_string()),
        );
    } else {
        let result = std::mem::take(&mut response.data);
        response.data = json!({
            "result": result,
            "selection_fingerprint": fingerprint,
        });
    }
    response
}

fn with_composition_tui_status(mut response: ApiSuccess, submitted: &Value) -> ApiSuccess {
    if !has_composition_plan_field(&response.data) {
        return response;
    }
    let secret_values = submitted_secret_ref_values(&response.data, submitted);
    redact_strings(&mut response.data, &secret_values);
    let summary = composition_tui_status(&response.data, submitted)
        .unwrap_or_else(|| json!({"error": COMPOSITION_TUI_MALFORMED}));
    if let Some(data) = response.data.as_object_mut() {
        data.insert(COMPOSITION_TUI_STATUS_KEY.to_string(), summary);
    }
    response
}

fn composition_tui_status(data: &Value, submitted: &Value) -> Option<Value> {
    let plan = data
        .get("composition_plan")
        .or_else(|| data.get("compositionPlan"))?
        .as_object()
        .filter(|plan| !plan.is_empty())?;
    let root_service_id = string_field(plan, "rootServiceId", "root_service_id")?;
    let plan_digest = string_field(plan, "planDigest", "plan_digest")?;
    let release_graph_digest = string_field(plan, "releaseGraphDigest", "release_graph_digest")?;
    let plan_nodes = value_field(plan, "nodes", "nodes")?.as_array()?;
    let nodes = plan_nodes
        .iter()
        .map(|node| composition_node_status(node, Some(root_service_id), submitted))
        .collect::<Option<Vec<_>>>()?;

    let mut status = Map::new();
    status.insert(
        "plan_digest".to_string(),
        Value::String(plan_digest.to_string()),
    );
    status.insert(
        "release_graph_digest".to_string(),
        Value::String(release_graph_digest.to_string()),
    );
    status.insert(
        "root_service_id".to_string(),
        Value::String(root_service_id.to_string()),
    );
    if let Some(valid) = data
        .get("composition_inputs_valid")
        .or_else(|| data.get("compositionInputsValid"))
        .and_then(Value::as_bool)
    {
        status.insert("inputs_valid".to_string(), Value::Bool(valid));
    }
    if let Some(error) = data
        .get("composition_input_error")
        .or_else(|| data.get("compositionInputError"))
        .and_then(Value::as_str)
        .filter(|error| !error.trim().is_empty())
    {
        let mut error = error.to_string();
        redact_text(&mut error, &submitted_secret_ref_values(data, submitted));
        status.insert("input_error".to_string(), Value::String(error));
    }
    status.insert("nodes".to_string(), Value::Array(nodes));
    status.insert(
        "install_input_editing".to_string(),
        Value::String("UNSUPPORTED".to_string()),
    );
    status.insert(
        "notice".to_string(),
        Value::String(COMPOSITION_TUI_NOTICE.to_string()),
    );
    Some(Value::Object(status))
}

fn has_composition_plan_field(data: &Value) -> bool {
    data.get("composition_plan")
        .or_else(|| data.get("compositionPlan"))
        .is_some_and(|plan| !plan.is_null())
}

fn composition_node_status(
    node: &Value,
    root_service_id: Option<&str>,
    submitted: &Value,
) -> Option<Value> {
    let node = node.as_object()?;
    let node_id = string_field(node, "nodeId", "node_id")?;
    let kind = string_field(node, "kind", "kind").unwrap_or("unknown");
    let mut status = Map::new();
    status.insert("node_id".to_string(), Value::String(node_id.to_string()));
    insert_string_field(
        &mut status,
        "service_id",
        value_field(node, "serviceId", "service_id"),
    );
    status.insert("kind".to_string(), Value::String(kind.to_string()));
    insert_string_field(&mut status, "name", value_field(node, "name", "name"));
    insert_string_field(
        &mut status,
        "resource_type",
        value_field(node, "resourceType", "resource_type"),
    );
    insert_string_field(
        &mut status,
        "lifecycle",
        value_field(node, "lifecycle", "lifecycle"),
    );

    if let Some(provider) = value_field(node, "provider", "provider") {
        status.insert(
            "provider".to_string(),
            provider_status(provider.as_object()?, node_id, submitted),
        );
    }

    let inputs = value_field(node, "unresolvedInputs", "unresolved_inputs")?
        .as_array()?
        .iter()
        .map(|declaration| {
            composition_input_status(node, node_id, kind, root_service_id, declaration, submitted)
        })
        .collect::<Option<Vec<_>>>()?;
    status.insert("unresolved_inputs".to_string(), Value::Array(inputs));
    Some(Value::Object(status))
}

fn provider_status(provider: &Map<String, Value>, node_id: &str, submitted: &Value) -> Value {
    let candidates = value_field(provider, "candidates", "candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|candidate| {
            let candidate = candidate.as_object()?;
            let provider_id = string_field(candidate, "providerId", "provider_id")?;
            let mut status = Map::new();
            status.insert(
                "provider_id".to_string(),
                Value::String(provider_id.to_string()),
            );
            status.insert(
                "kind".to_string(),
                Value::String(
                    string_field(candidate, "kind", "kind")
                        .unwrap_or("UNKNOWN")
                        .to_ascii_uppercase(),
                ),
            );
            insert_string_field(
                &mut status,
                "service_id",
                value_field(candidate, "serviceId", "service_id"),
            );
            Some(Value::Object(status))
        })
        .collect::<Vec<_>>();
    let planned_id = string_field(provider, "selectedProviderId", "selected_provider_id");
    let requested_id = submitted
        .get("inputs")
        .and_then(Value::as_object)
        .and_then(|inputs| inputs.get(node_id))
        .and_then(Value::as_object)
        .and_then(|inputs| {
            inputs
                .get("providerId")
                .or_else(|| inputs.get("provider_id"))
        })
        .and_then(Value::as_str)
        .filter(|provider_id| !provider_id.trim().is_empty());
    let selected_id = planned_id.or_else(|| {
        requested_id.filter(|requested| {
            candidates.iter().any(|candidate| {
                candidate.get("provider_id").and_then(Value::as_str) == Some(*requested)
            })
        })
    });
    let source = if planned_id.is_some() {
        "plan"
    } else if selected_id.is_some() {
        "request"
    } else if requested_id.is_some() {
        "invalid"
    } else {
        "none"
    };
    let selected = selected_id.map(|provider_id| {
        let kind = candidates
            .iter()
            .filter_map(Value::as_object)
            .find(|candidate| {
                candidate.get("provider_id").and_then(Value::as_str) == Some(provider_id)
            })
            .and_then(|candidate| candidate.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        json!({"provider_id": provider_id, "kind": kind, "source": source})
    });
    json!({
        "selected": selected,
        "selection_status": if source == "invalid" { "invalid" } else if selected_id.is_some() { "selected" } else { "unresolved" },
        "candidates": candidates,
    })
}

fn composition_input_status(
    node: &Map<String, Value>,
    node_id: &str,
    node_kind: &str,
    root_service_id: Option<&str>,
    declaration: &Value,
    submitted: &Value,
) -> Option<Value> {
    let declaration = declaration.as_object()?;
    let name = string_field(declaration, "key", "key")?;
    let value_type = string_field(declaration, "valueType", "value_type").unwrap_or("unknown");
    let required = value_field(declaration, "required", "required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sensitive = value_field(declaration, "sensitive", "sensitive")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| value_type.eq_ignore_ascii_case("secret-ref"));
    let provided = submitted_node_input(submitted, node_id, name)
        || submitted_legacy_input(
            submitted,
            node,
            node_kind,
            root_service_id,
            name,
            value_type,
        );
    Some(json!({
        "name": name,
        "type": value_type,
        "required": required,
        "sensitive": sensitive,
        "state": if provided { "provided" } else { "missing" },
    }))
}

fn submitted_node_input(submitted: &Value, node_id: &str, key: &str) -> bool {
    submitted
        .get("inputs")
        .and_then(Value::as_object)
        .and_then(|inputs| inputs.get(node_id))
        .and_then(Value::as_object)
        .and_then(|values| values.get(key))
        .is_some_and(value_is_present)
}

fn submitted_legacy_input(
    submitted: &Value,
    node: &Map<String, Value>,
    node_kind: &str,
    root_service_id: Option<&str>,
    key: &str,
    value_type: &str,
) -> bool {
    if node_kind.eq_ignore_ascii_case("config")
        && value_type.eq_ignore_ascii_case("json-object")
        && key == "config"
    {
        return submitted.get("config").is_some_and(value_is_present);
    }
    if !node_kind.eq_ignore_ascii_case("secret") && !value_type.eq_ignore_ascii_case("secret-ref") {
        return false;
    }
    if string_field(node, "serviceId", "service_id") != root_service_id {
        return false;
    }
    let secret_name = string_field(node, "name", "name").unwrap_or(key);
    submitted
        .get("secret_refs")
        .and_then(Value::as_object)
        .and_then(|references| references.get(secret_name).or_else(|| references.get(key)))
        .is_some_and(value_is_present)
}

fn value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        _ => true,
    }
}

fn submitted_secret_ref_values(data: &Value, submitted: &Value) -> Vec<String> {
    let mut values = submitted
        .get("secret_refs")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|references| references.values())
        .filter_map(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let plan_nodes = data
        .get("composition_plan")
        .or_else(|| data.get("compositionPlan"))
        .and_then(Value::as_object)
        .and_then(|plan| value_field(plan, "nodes", "nodes"))
        .and_then(Value::as_array);
    if let (Some(nodes), Some(inputs)) = (
        plan_nodes,
        submitted.get("inputs").and_then(Value::as_object),
    ) {
        for node in nodes {
            let Some(node) = node.as_object() else {
                continue;
            };
            let Some(node_id) = string_field(node, "nodeId", "node_id") else {
                continue;
            };
            let Some(submitted_values) = inputs.get(node_id).and_then(Value::as_object) else {
                continue;
            };
            for declaration in value_field(node, "unresolvedInputs", "unresolved_inputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(declaration) = declaration.as_object() else {
                    continue;
                };
                let sensitive = value_field(declaration, "sensitive", "sensitive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let Some(key) = string_field(declaration, "key", "key") else {
                    continue;
                };
                if sensitive
                    && let Some(value) = submitted_values.get(key).and_then(Value::as_str)
                    && !value.is_empty()
                {
                    values.push(value.to_string());
                }
            }
        }
    }
    values.sort();
    values.dedup();
    values
}

fn redact_strings(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(value) => redact_text(value, secrets),
        Value::Array(values) => {
            for value in values {
                redact_strings(value, secrets);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_strings(value, secrets);
            }
        }
        _ => {}
    }
}

fn redact_text(value: &mut String, secrets: &[String]) {
    for secret in secrets {
        if value.contains(secret) {
            *value = value.replace(secret, "[REDACTED]");
        }
    }
}

fn remote_response_detail(command: &RemoteCommand, data: &Value) -> String {
    if !matches!(command, RemoteCommand::StoreValidate { .. }) || !has_composition_plan_field(data)
    {
        return serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    }
    let Some(status) = data.get(COMPOSITION_TUI_STATUS_KEY) else {
        return COMPOSITION_TUI_MALFORMED.to_string();
    };
    if status.get("error").is_some() {
        return COMPOSITION_TUI_MALFORMED.to_string();
    }
    let summary = serde_json::to_string_pretty(status).unwrap_or_else(|_| status.to_string());
    format!("Composition status (read-only)\n{summary}")
}

fn value_field<'a>(
    object: &'a Map<String, Value>,
    camel_case: &str,
    snake_case: &str,
) -> Option<&'a Value> {
    object.get(camel_case).or_else(|| object.get(snake_case))
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    camel_case: &str,
    snake_case: &str,
) -> Option<&'a str> {
    value_field(object, camel_case, snake_case).and_then(Value::as_str)
}

fn insert_string_field(output: &mut Map<String, Value>, name: &str, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_str) {
        output.insert(name.to_string(), Value::String(value.to_string()));
    }
}

fn replacement_selection_body(
    deployment_id: &str,
    version: Option<&str>,
    catalog_source_id: Option<&str>,
    bindings: &[InstallApiBindingSelection],
    topologies: &[InstallTopologySelection],
) -> Value {
    let mut body = json!({
        "deployment_id": deployment_id,
        "version": version.unwrap_or_default(),
        "catalog_source_id": catalog_source_id.unwrap_or_default(),
        "bindings": bindings,
    });
    let cas = topologies
        .iter()
        .map(|topology| {
            json!({
                "topology_id": topology.topology_id,
                "topology_etag": topology.revision_id.as_ref().map(|revision| format!("\"{revision}\"")).unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    if cas.len() == 1 {
        body["topology_id"] = cas[0]["topology_id"].clone();
        body["topology_etag"] = cas[0]["topology_etag"].clone();
    } else if !cas.is_empty() {
        body["topologies"] = json!(cas);
    }
    body
}

fn replacement_context(
    client: &ApiClient,
    deployment_id: &str,
    explicit_bindings: &[InstallApiBindingSelection],
    explicit_topologies: &[InstallTopologySelection],
) -> Result<
    (
        Vec<InstallApiBindingSelection>,
        Vec<InstallTopologySelection>,
    ),
    ApiError,
> {
    let evidence = client.deployment_bindings(deployment_id)?;
    let bindings = if explicit_bindings.is_empty() {
        binding_selections_from_evidence(deployment_id, &evidence.data)?
    } else {
        explicit_bindings.to_vec()
    };
    let topologies = if explicit_topologies.is_empty() {
        let mut selections = Vec::new();
        for topology_id in active_topology_ids(&evidence.data) {
            let detail = client.topology(&topology_id)?;
            let revision_id = detail
                .data
                .pointer("/heads/applied_revision_id")
                .and_then(Value::as_str)
                .filter(|revision| !revision.trim().is_empty())
                .ok_or_else(|| {
                    ApiError::InvalidResponse(format!(
                        "deployment {deployment_id} has an active Binding in Topology {topology_id}, but no applied revision was returned"
                    ))
                })?;
            selections.push(InstallTopologySelection {
                topology_id,
                revision_id: Some(revision_id.to_string()),
            });
        }
        selections
    } else {
        explicit_topologies.to_vec()
    };
    Ok((bindings, topologies))
}

fn active_topology_ids(evidence: &Value) -> Vec<String> {
    let mut topology_ids = ["items", "provider_items"]
        .into_iter()
        .flat_map(|key| {
            evidence
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|binding| {
            binding
                .get("desired_state")
                .and_then(Value::as_str)
                .is_some_and(|state| state == "ACTIVE")
        })
        .filter_map(|binding| binding.get("topology_id").and_then(Value::as_str))
        .filter(|topology_id| !topology_id.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    topology_ids.sort();
    topology_ids.dedup();
    topology_ids
}

fn binding_selections_from_evidence(
    deployment_id: &str,
    evidence: &Value,
) -> Result<Vec<InstallApiBindingSelection>, ApiError> {
    let mut bindings = evidence
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|binding| {
            binding
                .get("desired_state")
                .and_then(Value::as_str)
                .is_some_and(|state| state == "ACTIVE")
        })
        .filter_map(|binding| {
            let name = binding
                .get("requirement_name")
                .or_else(|| binding.get("name"))?
                .as_str()?
                .trim();
            let provider = binding.get("provider_deployment_id")?.as_str()?.trim();
            (!name.is_empty() && !provider.is_empty()).then(|| InstallApiBindingSelection {
                name: name.to_string(),
                provider_deployment_id: provider.to_string(),
            })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| left.name.cmp(&right.name));
    if bindings.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(ApiError::InvalidResponse(format!(
            "deployment {deployment_id} returned duplicate active Binding requirements"
        )));
    }
    Ok(bindings)
}

fn split_command_line(source: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in source.trim().chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            character if character.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => token.push(character),
        }
    }
    if escaped || quote.is_some() {
        return Err("unterminated escape or quote in command".to_string());
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    if tokens.is_empty() {
        return Err(command_usage());
    }
    Ok(tokens)
}

fn command_usage() -> String {
    "commands: store catalogs|catalog add <id> <url> <required-key-id> [env:AUTH_TOKEN_VAR|-] [padded-base64-public-key|-]|catalog remove <id>|list/search|import <service> <node> [version|-] [catalog|-] [channel]|validate|install <service> <node> <version|-> <catalog|-> <channel|-> <requirement=provider,..|-> <topology@applied-revision|-> [pipeline-options.json|-]|upgrade|rollback <deployment> <version|-> <catalog|-> <requirement=provider,..|-> <topology@revision,..|->|release delete <service> <version>|uninstall <deployment>; topology list|get|draft <spec.json>|revisions|revision get/create|endpoint put/delete|link put/delete|validate <id> <spec.json>|diff|apply|rollback|status|export; operation list|plan <plan.json>|get|logs|events|confirm|apply|cancel|retry|rollback; node list|get|health|register|revoke-certificates|drain|remove; deployment list|get|health|bindings|start|stop|restart|uninstall; resource purge <claim> <node> <sha256-digest> <generation> <exact-confirmation> <reason>; diagnostic list|create|get|export".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_client::Capability;
    use std::collections::BTreeSet;

    #[test]
    fn remote_control_actions_are_unique_members_of_the_formal_v1_contract() {
        let actions = TUI_V1_CONTROL_ACTIONS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(actions.len(), TUI_V1_CONTROL_ACTIONS.len());
        for action in &actions {
            assert!(
                orchestrator_legacy::v1_action(action).is_some(),
                "TUI exposes non-contract action {action}"
            );
        }
        let published = orchestrator_legacy::V1_ACTIONS
            .iter()
            .map(|descriptor| descriptor.action_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actions, published,
            "TUI control surface must cover v1 exactly"
        );
    }

    #[test]
    fn command_parser_covers_control_parity_actions() {
        assert_eq!(
            RemoteCommand::parse(
                "store catalog add stable https://catalog.example/v2.json release-key"
            )
            .unwrap(),
            RemoteCommand::CatalogRegister {
                id: "stable".to_string(),
                url: "https://catalog.example/v2.json".to_string(),
                required_key_id: "release-key".to_string(),
                auth_secret_ref: None,
                public_key: None,
            }
        );
        assert_eq!(
            RemoteCommand::parse(
                "store catalog add private https://catalog.example/private.json release-key env:OJOS_CATALOG_TOKEN"
            )
            .unwrap(),
            RemoteCommand::CatalogRegister {
                id: "private".to_string(),
                url: "https://catalog.example/private.json".to_string(),
                required_key_id: "release-key".to_string(),
                auth_secret_ref: Some("env:OJOS_CATALOG_TOKEN".to_string()),
                public_key: None,
            }
        );
        assert_eq!(
            RemoteCommand::parse(
                "store catalog add bootstrap https://catalog.example/bootstrap.json bootstrap-key - AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            )
            .unwrap(),
            RemoteCommand::CatalogRegister {
                id: "bootstrap".to_string(),
                url: "https://catalog.example/bootstrap.json".to_string(),
                required_key_id: "bootstrap-key".to_string(),
                auth_secret_ref: None,
                public_key: Some(
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string()
                ),
            }
        );
        let delete = RemoteCommand::parse("store release delete gateway 1.2.3").unwrap();
        assert_eq!(
            delete,
            RemoteCommand::StoreReleaseDelete {
                service_id: "gateway".to_string(),
                version: "1.2.3".to_string(),
            }
        );
        assert_eq!(delete.capability().as_deref(), Some("release.delete"));
        let uninstall = RemoteCommand::parse("store uninstall deployment-1").unwrap();
        assert_eq!(
            uninstall,
            RemoteCommand::StoreUninstall {
                deployment_id: "deployment-1".to_string(),
            }
        );
        assert_eq!(
            uninstall.capability().as_deref(),
            Some("deployment.uninstall")
        );
        let digest = format!("sha256:{}", "b".repeat(64));
        let confirmation = format!("PURGE claim-1 {digest} GENERATION 3");
        let purge = RemoteCommand::parse(&format!(
            "resource purge claim-1 node-1 {digest} 3 \"{confirmation}\" \"retention period expired\""
        ))
        .unwrap();
        assert_eq!(
            purge,
            RemoteCommand::ResourcePurge {
                claim_id: "claim-1".to_string(),
                node_id: "node-1".to_string(),
                claim_digest: digest,
                generation: 3,
                confirmation,
                reason: "retention period expired".to_string(),
            }
        );
        assert_eq!(purge.capability().as_deref(), Some("resource.purge"));
        assert!(
            RemoteCommand::parse(
                "resource purge claim-1 node-1 sha256:bad 3 PURGE reason extra-field"
            )
            .is_err()
        );
        assert!(RemoteCommand::parse("store install gateway node-a 1.2.3").is_err());
        assert_eq!(
            RemoteCommand::parse("store install storage-service node-a 1.2.3 - - -").unwrap(),
            RemoteCommand::StoreInstall {
                service_id: "storage-service".to_string(),
                target_node_id: "node-a".to_string(),
                version: Some("1.2.3".to_string()),
                catalog_source_id: None,
                channel: None,
                bindings: Vec::new(),
                topology: None,
                pipeline_options_path: None,
            }
        );
        assert_eq!(
            RemoteCommand::parse("store install gateway node-a 1.2.3 - - - primary@rev-1").unwrap(),
            RemoteCommand::StoreInstall {
                service_id: "gateway".to_string(),
                target_node_id: "node-a".to_string(),
                version: Some("1.2.3".to_string()),
                catalog_source_id: None,
                channel: None,
                bindings: Vec::new(),
                topology: Some(InstallTopologySelection {
                    topology_id: "primary".to_string(),
                    revision_id: Some("rev-1".to_string()),
                }),
                pipeline_options_path: None,
            }
        );
        assert_eq!(
            RemoteCommand::parse(
                "store validate judge-worker node-b 1.0.0 stable stable judge_control=judge-a,storage_get=storage-a primary@revision-7 pipeline-options.json"
            )
            .unwrap(),
            RemoteCommand::StoreValidate {
                service_id: "judge-worker".to_string(),
                target_node_id: "node-b".to_string(),
                version: Some("1.0.0".to_string()),
                catalog_source_id: Some("stable".to_string()),
                channel: Some("stable".to_string()),
                bindings: vec![
                    InstallApiBindingSelection {
                        name: "judge_control".to_string(),
                        provider_deployment_id: "judge-a".to_string(),
                    },
                    InstallApiBindingSelection {
                        name: "storage_get".to_string(),
                        provider_deployment_id: "storage-a".to_string(),
                    },
                ],
                topology: Some(InstallTopologySelection {
                    topology_id: "primary".to_string(),
                    revision_id: Some("revision-7".to_string()),
                }),
                pipeline_options_path: Some("pipeline-options.json".to_string()),
            }
        );
        assert!(RemoteCommand::parse(
            "store install judge-worker node-b - - - judge_control=judge-a,judge_control=judge-b primary"
        )
        .is_err());
        assert!(
            RemoteCommand::parse(
                "store install judge-worker node-b - - - judge_control=judge-a primary"
            )
            .is_err()
        );
        assert_eq!(
            RemoteCommand::parse("topology endpoint put main endpoint-a endpoint.json '\"rev-7\"'")
                .unwrap(),
            RemoteCommand::TopologyEndpointPut {
                topology_id: "main".to_string(),
                endpoint_id: "endpoint-a".to_string(),
                spec_path: "endpoint.json".to_string(),
                if_match: "\"rev-7\"".to_string(),
            }
        );
        assert_eq!(
            RemoteCommand::parse("topology link delete main endpoint-a endpoint-b '\"rev-8\"'")
                .unwrap(),
            RemoteCommand::TopologyLinkDelete {
                topology_id: "main".to_string(),
                source_endpoint: "endpoint-a".to_string(),
                target_endpoint: "endpoint-b".to_string(),
                if_match: "\"rev-8\"".to_string(),
            }
        );
        assert_eq!(
            RemoteCommand::parse("topology rollback main rev-3 '\"rev-7\"'").unwrap(),
            RemoteCommand::TopologyAction {
                topology_id: "main".to_string(),
                action: "rollback".to_string(),
                if_match: Some("\"rev-7\"".to_string()),
                target_revision: Some("rev-3".to_string()),
                spec_path: None,
                from_revision: None,
                to_revision: None,
            }
        );
        assert_eq!(
            RemoteCommand::parse("operation retry op-42").unwrap(),
            RemoteCommand::OperationAction {
                operation_id: "op-42".to_string(),
                action: "retry".to_string(),
            }
        );
        assert_eq!(
            RemoteCommand::parse("node drain edge-1").unwrap(),
            RemoteCommand::NodeDrain {
                node_id: "edge-1".to_string(),
            }
        );
        assert_eq!(
            RemoteCommand::parse("store upgrade dep-1 2.0.0 stable").unwrap(),
            RemoteCommand::StoreUpgrade {
                deployment_id: "dep-1".to_string(),
                version: Some("2.0.0".to_string()),
                catalog_source_id: Some("stable".to_string()),
                bindings: Vec::new(),
                topologies: Vec::new(),
            }
        );
        assert_eq!(
            RemoteCommand::parse("store import gateway node-a 1.2.3 stable nightly").unwrap(),
            RemoteCommand::StoreImport {
                service_id: "gateway".to_string(),
                target_node_id: "node-a".to_string(),
                version: Some("1.2.3".to_string()),
                catalog_source_id: Some("stable".to_string()),
                channel: Some("nightly".to_string()),
            }
        );
        assert!(
            RemoteCommand::parse("store import https://downloads.example/release.yaml deadbeef")
                .is_err()
        );
        assert_eq!(
            RemoteCommand::parse("topology validate main topology.json").unwrap(),
            RemoteCommand::TopologyAction {
                topology_id: "main".to_string(),
                action: "validate".to_string(),
                if_match: None,
                target_revision: None,
                spec_path: Some("topology.json".to_string()),
                from_revision: None,
                to_revision: None,
            }
        );
        assert!(matches!(
            RemoteCommand::parse("operation events op-42 cursor-1").unwrap(),
            RemoteCommand::OperationEvents { .. }
        ));
        assert!(matches!(
            RemoteCommand::parse("node health edge-1").unwrap(),
            RemoteCommand::NodeHealth { .. }
        ));
        assert!(matches!(
            RemoteCommand::parse("deployment health dep-1").unwrap(),
            RemoteCommand::DeploymentHealth { .. }
        ));
        assert_eq!(
            RemoteCommand::parse("deployment bindings worker-b").unwrap(),
            RemoteCommand::DeploymentBindings {
                deployment_id: "worker-b".to_string(),
            }
        );
        assert_eq!(
            RemoteCommand::parse("node register edge-1 10.0.0.8 standalone 900").unwrap(),
            RemoteCommand::NodeRegister {
                node_id: "edge-1".to_string(),
                host_ip: "10.0.0.8".to_string(),
                role: Some("standalone".to_string()),
                ttl_seconds: Some(900),
            }
        );
        assert!(matches!(
            RemoteCommand::parse("diagnostic export diag-1 markdown").unwrap(),
            RemoteCommand::DiagnosticExport { .. }
        ));
        assert_eq!(
            RemoteCommand::parse("diagnostic create").unwrap(),
            RemoteCommand::DiagnosticCreate
        );
        assert!(RemoteCommand::parse("diagnostic create Operation op-42").is_err());
    }

    #[test]
    fn deployment_rows_read_the_durable_runtime_projection_shape() {
        let row = RemoteRow::from_value(
            RemotePage::Deployments,
            json!({
                "node_id": "node-a",
                "instance": {
                    "deployment_id": "dep-1",
                    "service_id": "gateway",
                    "observed_state": "RUNNING"
                }
            }),
        );
        assert_eq!(row.id, "dep-1");
        assert_eq!(row.status, "RUNNING");
        assert_eq!(row.summary, "gateway");
    }

    #[test]
    fn optional_store_selection_fields_use_backend_defaults_instead_of_json_null() {
        let pipeline = StorePipelineOptions {
            start: false,
            migration_policy: "DRY_RUN".to_string(),
            gateway_node_id: Some("gateway-a".to_string()),
            config: json!({"namespace": "contest"}),
            secret_refs: std::collections::BTreeMap::from([(
                "signing_key".to_string(),
                "secrets/judge/signing-key".to_string(),
            )]),
        };
        assert_eq!(
            catalog_selection_body("gateway", "node-a", None, None, None),
            json!({
                "service_id": "gateway",
                "target_node_id": "node-a",
                "version": "",
                "catalog_source_id": "",
                "channel": "stable",
            })
        );
        assert_eq!(
            catalog_binding_selection_body(
                "judge-worker",
                "node-b",
                None,
                None,
                None,
                StoreBindingSelection {
                    bindings: &[InstallApiBindingSelection {
                        name: "judge_control".to_string(),
                        provider_deployment_id: "judge-a".to_string(),
                    }],
                    topology: Some(&InstallTopologySelection {
                        topology_id: "primary".to_string(),
                        revision_id: Some("revision-7".to_string()),
                    }),
                    pipeline: &pipeline,
                },
            ),
            json!({
                "service_id": "judge-worker",
                "target_node_id": "node-b",
                "version": "",
                "catalog_source_id": "",
                "channel": "stable",
                "start": false,
                "migration_policy": "DRY_RUN",
                "gateway_node_id": "gateway-a",
                "config": {"namespace": "contest"},
                "secret_refs": {"signing_key": "secrets/judge/signing-key"},
                "bindings": [{
                    "name": "judge_control",
                    "provider_deployment_id": "judge-a",
                }],
                "topology_id": "primary",
                "topology_etag": "\"revision-7\"",
            })
        );
        let confirmed = catalog_binding_selection_body(
            "judge-worker",
            "node-b",
            Some("1.0.0"),
            Some("stable"),
            Some("stable"),
            StoreBindingSelection {
                bindings: &[InstallApiBindingSelection {
                    name: "judge_control".to_string(),
                    provider_deployment_id: "judge-a".to_string(),
                }],
                topology: Some(&InstallTopologySelection {
                    topology_id: "primary".to_string(),
                    revision_id: Some("revision-7".to_string()),
                }),
                pipeline: &pipeline,
            },
        );
        let fingerprint = selection_fingerprint(&confirmed);
        assert!(fingerprint.starts_with("fnv1a64:"));
        assert_eq!(fingerprint.len(), 24);
        let mut changed = confirmed;
        changed["bindings"][0]["provider_deployment_id"] = json!("judge-b");
        assert_ne!(fingerprint, selection_fingerprint(&changed));
        assert_eq!(
            replacement_selection_body("dep-1", None, None, &[], &[]),
            json!({
                "deployment_id": "dep-1",
                "version": "",
                "catalog_source_id": "",
                "bindings": [],
            })
        );
        assert_eq!(
            RemoteCommand::parse(
                "store upgrade dep-1 2.0.0 stable storage_get=storage-b contest@rev-9"
            )
            .unwrap(),
            RemoteCommand::StoreUpgrade {
                deployment_id: "dep-1".to_string(),
                version: Some("2.0.0".to_string()),
                catalog_source_id: Some("stable".to_string()),
                bindings: vec![InstallApiBindingSelection {
                    name: "storage_get".to_string(),
                    provider_deployment_id: "storage-b".to_string(),
                }],
                topologies: vec![InstallTopologySelection {
                    topology_id: "contest".to_string(),
                    revision_id: Some("rev-9".to_string()),
                }],
            }
        );
    }

    #[test]
    fn store_pipeline_options_are_strict_normalized_and_part_of_the_selection() {
        let parsed: StorePipelineOptions = serde_json::from_value(json!({
            "start": false,
            "migration_policy": "dry_run",
            "gateway_node_id": " gateway-a ",
            "config": {"namespace": "contest"},
            "secret_refs": {" signing_key ": " secrets/judge/signing-key "}
        }))
        .unwrap();
        let normalized = normalize_store_pipeline_options(parsed).unwrap();
        assert!(!normalized.start);
        assert_eq!(normalized.migration_policy, "DRY_RUN");
        assert_eq!(normalized.gateway_node_id.as_deref(), Some("gateway-a"));
        assert_eq!(
            normalized
                .secret_refs
                .get("signing_key")
                .map(String::as_str),
            Some("secrets/judge/signing-key")
        );
        assert!(matches!(
            normalize_store_pipeline_options(StorePipelineOptions {
                config: json!([]),
                ..StorePipelineOptions::default()
            }),
            Err(ApiError::InvalidRequest(message)) if message.contains("config must be a JSON object")
        ));
        assert!(
            serde_json::from_value::<StorePipelineOptions>(json!({
                "unknown": true
            }))
            .is_err()
        );
    }

    #[test]
    fn composition_validate_status_is_concise_and_redacts_secret_references() {
        let root_secret = "vault://DO-NOT-PRINT-root-signing-key";
        let node_secret = "vault://DO-NOT-PRINT-node-token";
        let data = json!({
            "valid": false,
            "composition_inputs_valid": false,
            "composition_input_error": format!("secret reference {node_secret} is invalid"),
            "composition_plan": {
                "schemaVersion": "ojos.dev/composition-plan/v1",
                "rootServiceId": "contest-service",
                "planDigest": "sha256:plan",
                "releaseGraphDigest": "sha256:graph",
                "nodes": [
                    {
                        "nodeId": "api-node",
                        "serviceId": "contest-service",
                        "kind": "api-binding",
                        "name": "storage",
                        "provider": {
                            "selectedProviderId": "storage-package",
                            "candidates": [{
                                "providerId": "storage-package",
                                "kind": "PACKAGE",
                                "serviceId": "storage-service"
                            }]
                        },
                        "unresolvedInputs": []
                    },
                    {
                        "nodeId": "claim-node",
                        "serviceId": "contest-service",
                        "kind": "resource-claim",
                        "name": "database",
                        "resourceType": "postgresql.database/v1",
                        "lifecycle": "RETAIN",
                        "provider": {
                            "candidates": [
                                {"providerId": "postgres-local", "kind": "MANAGED"},
                                {"providerId": "postgres-external", "kind": "EXTERNAL"}
                            ]
                        },
                        "unresolvedInputs": [{
                            "key": "providerId",
                            "valueType": "provider-id",
                            "required": true,
                            "sensitive": false,
                            "allowedValues": ["postgres-local", "postgres-external"]
                        }]
                    },
                    {
                        "nodeId": "root-secret",
                        "serviceId": "contest-service",
                        "kind": "secret",
                        "name": "signing-key",
                        "unresolvedInputs": [{
                            "key": "secretRef",
                            "valueType": "secret-ref",
                            "required": true,
                            "sensitive": true
                        }]
                    },
                    {
                        "nodeId": "dependency-secret",
                        "serviceId": "worker-service",
                        "kind": "secret",
                        "name": "signing-key",
                        "unresolvedInputs": [{
                            "key": "secretRef",
                            "valueType": "secret-ref",
                            "required": true,
                            "sensitive": true
                        }]
                    },
                    {
                        "nodeId": "conditional-secret",
                        "serviceId": "contest-service",
                        "kind": "secret",
                        "name": "invite-key",
                        "unresolvedInputs": [{
                            "key": "secretRef",
                            "valueType": "secret-ref",
                            "required": false,
                            "sensitive": true
                        }]
                    }
                ],
                "edges": []
            }
        });
        let submitted = json!({
            "inputs": {
                "claim-node": {"providerId": "postgres-local"},
                "conditional-secret": {"secretRef": node_secret}
            },
            "secret_refs": {"signing-key": root_secret}
        });
        let response = ApiSuccess {
            status: 200,
            data,
            meta: crate::api_client::ResponseMeta {
                request_id: "req-composition".to_string(),
                api_version: "v1".to_string(),
                next_cursor: None,
            },
            etag: None,
        };

        let response = with_composition_tui_status(response, &submitted);
        let summary = &response.data[COMPOSITION_TUI_STATUS_KEY];
        assert_eq!(summary["plan_digest"], "sha256:plan");
        assert_eq!(summary["release_graph_digest"], "sha256:graph");
        assert_eq!(summary["root_service_id"], "contest-service");
        assert_eq!(summary["inputs_valid"], false);
        assert_eq!(summary["install_input_editing"], "UNSUPPORTED");
        assert!(
            summary["notice"]
                .as_str()
                .unwrap()
                .contains("use the Web manager")
        );

        let node = |id: &str| {
            summary["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["node_id"] == id)
                .unwrap()
        };
        assert_eq!(node("api-node")["provider"]["selected"]["kind"], "PACKAGE");
        assert_eq!(node("api-node")["provider"]["selected"]["source"], "plan");
        assert_eq!(
            node("claim-node")["provider"]["candidates"][0]["kind"],
            "MANAGED"
        );
        assert_eq!(
            node("claim-node")["provider"]["candidates"][1]["kind"],
            "EXTERNAL"
        );
        assert_eq!(
            node("claim-node")["resource_type"],
            "postgresql.database/v1"
        );
        assert_eq!(node("claim-node")["lifecycle"], "RETAIN");
        assert_eq!(
            node("claim-node")["provider"]["selected"]["provider_id"],
            "postgres-local"
        );
        assert_eq!(
            node("claim-node")["provider"]["selected"]["source"],
            "request"
        );
        assert_eq!(
            node("claim-node")["unresolved_inputs"][0]["state"],
            "provided"
        );
        assert_eq!(
            node("root-secret")["unresolved_inputs"][0]["state"],
            "provided"
        );
        assert_eq!(
            node("dependency-secret")["unresolved_inputs"][0]["state"],
            "missing"
        );
        assert_eq!(
            node("conditional-secret")["unresolved_inputs"][0]["state"],
            "provided"
        );

        let serialized = serde_json::to_string(&response.data).unwrap();
        assert!(!serialized.contains(root_secret));
        assert!(!serialized.contains(node_secret));
        assert!(serialized.contains("[REDACTED]"));
        let detail = remote_response_detail(
            &RemoteCommand::StoreValidate {
                service_id: "contest-service".to_string(),
                target_node_id: "node-a".to_string(),
                version: None,
                catalog_source_id: None,
                channel: None,
                bindings: Vec::new(),
                topology: None,
                pipeline_options_path: None,
            },
            &response.data,
        );
        assert!(detail.contains("Composition status (read-only)"));
        assert!(detail.contains(COMPOSITION_TUI_NOTICE));
        assert!(!detail.contains("Raw API response"));
        assert!(!detail.contains("composition_plan"));
        assert!(!detail.contains(root_secret));
        assert!(!detail.contains(node_secret));
    }

    #[test]
    fn composition_status_fails_closed_for_malformed_plan_and_invalid_provider() {
        let command = RemoteCommand::StoreValidate {
            service_id: "contest-service".to_string(),
            target_node_id: "node-a".to_string(),
            version: None,
            catalog_source_id: None,
            channel: None,
            bindings: Vec::new(),
            topology: None,
            pipeline_options_path: None,
        };
        let malformed = json!({
            "composition_plan": {"planDigest": "sha256:plan"},
            "private": "DO-NOT-DISPLAY"
        });
        assert_eq!(
            remote_response_detail(&command, &malformed),
            COMPOSITION_TUI_MALFORMED
        );
        let malformed_response = ApiSuccess {
            status: 200,
            data: malformed,
            meta: crate::api_client::ResponseMeta {
                request_id: "req-malformed".to_string(),
                api_version: "v1".to_string(),
                next_cursor: None,
            },
            etag: None,
        };
        let malformed_response = with_composition_tui_status(malformed_response, &json!({}));
        assert_eq!(
            remote_response_detail(&command, &malformed_response.data),
            COMPOSITION_TUI_MALFORMED
        );

        let attacker_value = "attacker-controlled\nprovider";
        let data = json!({
            "composition_inputs_valid": false,
            "composition_plan": {
                "rootServiceId": "contest-service",
                "planDigest": "sha256:plan",
                "releaseGraphDigest": "sha256:graph",
                "nodes": [{
                    "nodeId": "claim-node",
                    "serviceId": "contest-service",
                    "kind": "resource-claim",
                    "name": "database",
                    "resourceType": "postgresql.database/v1",
                    "lifecycle": "RETAIN",
                    "provider": {
                        "candidates": [{"providerId": "postgres-local", "kind": "MANAGED"}]
                    },
                    "unresolvedInputs": [{
                        "key": "providerId",
                        "valueType": "provider-id",
                        "required": true,
                        "sensitive": false,
                        "allowedValues": ["postgres-local"]
                    }]
                }]
            }
        });
        let summary = composition_tui_status(
            &data,
            &json!({"inputs": {"claim-node": {"providerId": attacker_value}}}),
        )
        .unwrap();
        assert_eq!(
            summary["nodes"][0]["provider"]["selection_status"],
            "invalid"
        );
        assert!(summary["nodes"][0]["provider"]["selected"].is_null());
        assert!(
            !serde_json::to_string(&summary)
                .unwrap()
                .contains(attacker_value)
        );
    }

    #[test]
    fn non_composition_response_detail_preserves_existing_raw_behavior() {
        let data = json!({"operation_id": "op-1", "status": "RUNNING"});
        assert_eq!(
            remote_response_detail(&RemoteCommand::Capabilities, &data),
            serde_json::to_string_pretty(&data).unwrap()
        );
        let response = ApiSuccess {
            status: 200,
            data: data.clone(),
            meta: crate::api_client::ResponseMeta {
                request_id: "req-raw".to_string(),
                api_version: "v1".to_string(),
                next_cursor: None,
            },
            etag: None,
        };
        assert_eq!(with_composition_tui_status(response, &json!({})).data, data);
    }

    #[test]
    fn replacement_preserves_active_consumer_bindings_from_deployment_evidence() {
        let evidence = json!({
            "items": [
                {
                    "requirement_name": "storage_get",
                    "provider_deployment_id": "storage-a",
                    "desired_state": "ACTIVE",
                    "topology_id": "contest"
                },
                {
                    "requirement_name": "judge_control",
                    "provider_deployment_id": "judge-a",
                    "desired_state": "ACTIVE",
                    "topology_id": "contest"
                },
                {
                    "requirement_name": "retired",
                    "provider_deployment_id": "old-a",
                    "desired_state": "INACTIVE"
                }
            ],
            "provider_items": [{
                "requirement_name": "problem_events",
                "provider_deployment_id": "worker-b",
                "desired_state": "ACTIVE",
                "topology_id": "secondary"
            }]
        });
        let bindings = binding_selections_from_evidence("worker-b", &evidence).unwrap();
        assert_eq!(
            bindings,
            vec![
                InstallApiBindingSelection {
                    name: "judge_control".to_string(),
                    provider_deployment_id: "judge-a".to_string(),
                },
                InstallApiBindingSelection {
                    name: "storage_get".to_string(),
                    provider_deployment_id: "storage-a".to_string(),
                },
            ]
        );
        assert_eq!(
            replacement_selection_body("worker-b", None, None, &bindings, &[])["bindings"],
            json!([
                {"name":"judge_control","provider_deployment_id":"judge-a"},
                {"name":"storage_get","provider_deployment_id":"storage-a"}
            ])
        );
        assert_eq!(
            active_topology_ids(&evidence),
            vec!["contest".to_string(), "secondary".to_string()]
        );
    }

    #[test]
    fn operation_fixture_maps_rows_without_inventing_defaults() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/operations.json")).unwrap();
        let rows = rows_from_response(RemotePage::Operations, &fixture["data"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "op-queued");
        assert_eq!(rows[0].status, "QUEUED");
        assert_eq!(rows[1].status, "NEEDS_ATTENTION");
    }

    #[test]
    fn help_hides_actions_not_published_by_server() {
        let capabilities = CapabilitySet::from_entries([Capability {
            action: "operation.retry".to_string(),
            target_type: "Operation".to_string(),
            capability_status: "STORE_BACKED".to_string(),
            required_permission: "orchestrator.operate".to_string(),
        }]);
        let help = page_help(RemotePage::Operations, &capabilities);
        assert!(help.contains("y retry"));
        assert!(!help.contains("c cancel"));
        assert!(!help.contains("b rollback"));
    }

    #[test]
    fn quoted_command_tokens_preserve_etag_quotes() {
        assert_eq!(
            split_command_line("topology apply main '\"rev-2\"'").unwrap(),
            vec!["topology", "apply", "main", "\"rev-2\""]
        );
        assert!(split_command_line("operation get 'unterminated").is_err());
    }
}
