use crate::{
    Endpoint, Link, Operation, OperationLogRecord, OrchestratorError, OrchestratorStore,
    PgOrchestratorStore, Result, ServiceManifest, ServiceSet, SharedSchemas, Topology,
    build_operation_workbench, build_topology, default_action_request, expand_set,
    load_shared_schemas, new_operation_workbench_session, plan_action_preview,
    run_operation_workbench_flow, validate_action_catalog, validate_endpoint_id,
    validate_service_manifest_file, validate_service_set_file,
};
use crate::{OperationWorkbench, OperationWorkbenchSession};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestratorView {
    pub schemas: SharedSchemas,
    pub services: Vec<ServiceViewRow>,
    pub sets: Vec<SetViewRow>,
    pub endpoints: Vec<EndpointViewRow>,
    pub links: Vec<LinkViewRow>,
    pub operations: Vec<OperationViewRow>,
    pub operation_workbench: Option<OperationWorkbenchView>,
    pub logs: Vec<LogViewRow>,
    pub diagnostics: Vec<DiagnosticViewRow>,
    pub warnings: Vec<String>,
}

impl OperationWorkbenchView {
    pub fn from_session(session: &OperationWorkbenchSession) -> Self {
        operation_workbench_view_from_parts(&session.workbench, session)
    }
}

pub fn merge_operation_workbench_session_into_view(
    view: &mut OrchestratorView,
    session: &OperationWorkbenchSession,
) {
    let operation_id = session.current_operation.operation_id.clone();
    let mut log_counts = HashMap::new();
    log_counts.insert(operation_id.clone(), session.logs.len());
    if let Some(row) = operation_store_rows(
        std::slice::from_ref(&session.current_operation),
        &log_counts,
    )
    .into_iter()
    .next()
    {
        if let Some(existing) = view.operations.iter_mut().find(|existing| {
            existing.operation_id == operation_id
                || (existing.action == session.current_operation.action
                    && existing.status == "CATALOG")
        }) {
            *existing = row;
        } else {
            view.operations.insert(0, row);
        }
    }

    view.logs.retain(|log| log.operation_id != operation_id);
    view.logs.extend(
        session
            .logs
            .iter()
            .map(|record| operation_log_row(&session.current_operation, record)),
    );
    view.operation_workbench = Some(OperationWorkbenchView::from_session(session));
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceViewRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub endpoint: String,
    pub runtime: String,
    pub ui: String,
    pub health: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetViewRow {
    pub id: String,
    pub name: String,
    pub services: String,
    pub links: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointViewRow {
    pub endpoint: String,
    pub service_id: String,
    pub protocol: String,
    pub expose: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkViewRow {
    pub from: String,
    pub to: String,
    pub protocol: String,
    pub auth_mode: String,
    pub scope: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationViewRow {
    pub operation_id: String,
    pub action: String,
    pub target: String,
    pub status: String,
    pub risk: String,
    pub plan_required: String,
    pub mode: String,
    pub fields: String,
    pub preview_target: String,
    pub preview_steps: String,
    pub preview_confirmation: String,
    pub result: String,
    pub error: String,
    pub log_count: usize,
    pub summary: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbenchView {
    pub selected_action: String,
    pub operation_id: String,
    pub target: String,
    pub fields: String,
    pub editable_fields: Vec<OperationWorkbenchFieldView>,
    pub preview_steps: String,
    pub requires_confirmation: String,
    pub can_apply: String,
    pub rollback: String,
    pub current_status: String,
    pub result_status: String,
    pub log_count: usize,
    pub warnings: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbenchFieldView {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogViewRow {
    pub source_id: String,
    pub service_id: String,
    pub endpoint: String,
    pub operation_id: String,
    pub level: String,
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticViewRow {
    pub target: String,
    pub status: String,
    pub summary: String,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrchestratorViewPage {
    Overview,
    Services,
    Sets,
    Endpoints,
    Links,
    Operations,
    Topology,
    Logs,
    Diagnostics,
}

impl OrchestratorViewPage {
    pub const ALL: [Self; 9] = [
        Self::Overview,
        Self::Services,
        Self::Sets,
        Self::Endpoints,
        Self::Links,
        Self::Operations,
        Self::Topology,
        Self::Logs,
        Self::Diagnostics,
    ];

    pub fn all() -> &'static [Self] {
        &Self::ALL
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "总览",
            Self::Services => "Service",
            Self::Sets => "Set",
            Self::Endpoints => "Endpoint",
            Self::Links => "Link",
            Self::Operations => "Operation",
            Self::Topology => "Topology",
            Self::Logs => "LogView",
            Self::Diagnostics => "DiagnosticReport",
        }
    }

    pub fn core_object(self) -> Option<&'static str> {
        match self {
            Self::Overview => None,
            Self::Services => Some("Service"),
            Self::Sets => Some("Set"),
            Self::Endpoints => Some("Endpoint"),
            Self::Links => Some("Link"),
            Self::Operations => Some("Operation"),
            Self::Topology => Some("Topology"),
            Self::Logs => Some("LogView"),
            Self::Diagnostics => Some("DiagnosticReport"),
        }
    }

    pub fn key(self) -> Option<char> {
        match self {
            Self::Overview => Some('1'),
            Self::Services => Some('2'),
            Self::Sets => Some('3'),
            Self::Endpoints => Some('4'),
            Self::Links => Some('5'),
            Self::Operations => Some('6'),
            Self::Topology => Some('7'),
            Self::Logs => Some('8'),
            Self::Diagnostics => Some('9'),
        }
    }
}

pub fn load_orchestrator_view(repo_root: &Path) -> Result<OrchestratorView> {
    if std::env::var(PgOrchestratorStore::ENV_NAME).is_ok() {
        let schemas = load_shared_schemas(repo_root)?;
        match PgOrchestratorStore::from_env()
            .and_then(|store| load_orchestrator_view_from_store(schemas.clone(), &store))
        {
            Ok(view) => return Ok(view),
            Err(err) => {
                let mut view = load_orchestrator_view_from_repo(repo_root)?;
                view.warnings.push(format!(
                    "ORCHESTRATOR_DATABASE_URL store unavailable, using repo view: {err}"
                ));
                return Ok(view);
            }
        }
    }
    load_orchestrator_view_from_repo(repo_root)
}

pub fn load_orchestrator_view_from_store<S: OrchestratorStore>(
    schemas: SharedSchemas,
    store: &S,
) -> Result<OrchestratorView> {
    let services = store.services()?;
    let sets = store.sets()?;
    let endpoints = store.endpoints()?;
    let links = store.links()?;
    let operations = store.operations()?;
    let logs = store.log_views()?;
    let operation_logs = operation_log_rows(store, &operations)?;
    let operation_log_counts = operation_log_counts(&operation_logs);
    let diagnostics = store.diagnostic_reports()?;
    Ok(OrchestratorView {
        schemas,
        services: services.iter().map(service_model_row).collect(),
        sets: set_rows(&sets),
        endpoints: endpoints.iter().map(endpoint_model_row).collect(),
        links: links.iter().map(link_model_row).collect(),
        operations: operation_store_rows(&operations, &operation_log_counts),
        operation_workbench: None,
        logs: logs
            .iter()
            .map(log_model_row)
            .chain(operation_logs)
            .collect(),
        diagnostics: diagnostics.iter().map(diagnostic_model_row).collect(),
        warnings: Vec::new(),
    })
}

fn load_orchestrator_view_from_repo(repo_root: &Path) -> Result<OrchestratorView> {
    let mut warnings = Vec::new();
    let schemas = load_shared_schemas(repo_root)?;
    let manifests = load_service_manifests(repo_root, &mut warnings)?;
    let sets = load_sets(repo_root, &mut warnings)?;

    let mut services = manifests.iter().map(service_row).collect::<Vec<_>>();
    services.sort_by(|left, right| left.id.cmp(&right.id));

    let endpoint_models = endpoint_models(&manifests, &sets);
    let link_models = link_models(&sets);
    let topology = topology_model(&manifests, &sets, &endpoint_models, &link_models).ok();
    let operations = operation_rows(
        &schemas,
        &manifests,
        &sets,
        &endpoint_models,
        topology.as_ref(),
    );
    let operation_workbench = operation_workbench_view(
        &schemas,
        &manifests,
        &sets,
        &endpoint_models,
        topology.as_ref(),
    );
    let diagnostics = diagnostic_rows(&manifests, &sets, &warnings);

    Ok(OrchestratorView {
        schemas,
        services,
        sets: set_rows(&sets),
        endpoints: endpoint_rows(&manifests, &sets),
        links: link_rows(&sets),
        operations,
        operation_workbench,
        logs: log_rows(&manifests),
        diagnostics,
        warnings,
    })
}

fn load_service_manifests(
    repo_root: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<(ServiceManifest, PathBuf)>> {
    let mut rows = Vec::new();
    let services_dir = repo_root.join("services");
    if !services_dir.is_dir() {
        warnings.push("services/ 目录不存在".to_string());
        return Ok(rows);
    }
    for entry in fs::read_dir(&services_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let rel = Path::new("services")
            .join(entry.file_name())
            .join("service.yaml");
        if !repo_root.join(&rel).is_file() {
            continue;
        }
        match validate_service_manifest_file(repo_root, &rel) {
            Ok(manifest) => rows.push((manifest, rel)),
            Err(err) => warnings.push(format!("{}: {}", slash_path(&rel), err)),
        }
    }
    Ok(rows)
}

fn load_sets(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<ServiceSet>> {
    let mut sets = Vec::new();
    let sets_dir = repo_root.join("sets");
    if !sets_dir.is_dir() {
        warnings.push("sets/ 目录不存在".to_string());
        return Ok(sets);
    }
    for entry in fs::read_dir(&sets_dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("yaml") {
            continue;
        }
        let rel = Path::new("sets").join(entry.file_name());
        match validate_service_set_file(repo_root, &rel) {
            Ok(set) => sets.push(set),
            Err(err) => warnings.push(format!("{}: {}", slash_path(&rel), err)),
        }
    }
    sets.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(sets)
}

fn service_row((manifest, _rel): &(ServiceManifest, PathBuf)) -> ServiceViewRow {
    service_model_row(manifest)
}

fn service_model_row(manifest: &ServiceManifest) -> ServiceViewRow {
    ServiceViewRow {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        kind: manifest.kind.clone(),
        endpoint: endpoint_from_service(manifest),
        runtime: format!("{:?}", manifest.runtime.mode),
        ui: enabled_text(manifest.ui.enabled),
        health: manifest.health.checks.join(","),
    }
}

fn endpoint_model_row(endpoint: &Endpoint) -> EndpointViewRow {
    EndpointViewRow {
        endpoint: endpoint.endpoint.clone(),
        service_id: endpoint.service_id.clone(),
        protocol: endpoint.protocol.clone(),
        expose: endpoint.health.clone(),
        source: "store".to_string(),
    }
}

fn link_model_row(link: &Link) -> LinkViewRow {
    LinkViewRow {
        from: link.source_endpoint.clone(),
        to: link.target_endpoint.clone(),
        protocol: link.protocol.clone(),
        auth_mode: link.auth_mode.clone(),
        scope: link.scope.clone(),
        source: link.health.clone(),
    }
}

fn set_rows(sets: &[ServiceSet]) -> Vec<SetViewRow> {
    sets.iter()
        .map(|set| {
            let expanded = expand_set(set);
            SetViewRow {
                id: set.id.clone(),
                name: set.name.clone(),
                services: expanded.services.len().to_string(),
                links: expanded.default_links.len().to_string(),
                scope: if set.id == "judge-worker-node" {
                    "受限运行环境".to_string()
                } else {
                    "编排器管理".to_string()
                },
            }
        })
        .collect()
}

fn endpoint_rows(
    manifests: &[(ServiceManifest, PathBuf)],
    sets: &[ServiceSet],
) -> Vec<EndpointViewRow> {
    let mut rows = manifests
        .iter()
        .map(|(manifest, _path)| EndpointViewRow {
            endpoint: endpoint_from_service(manifest),
            service_id: manifest.id.clone(),
            protocol: manifest.endpoint.protocol.clone(),
            expose: enabled_text(manifest.endpoint.expose),
            source: "service.yaml".to_string(),
        })
        .collect::<Vec<_>>();
    for set in sets {
        for endpoint in &set.default_endpoints {
            rows.push(EndpointViewRow {
                endpoint: format!("0.0.0.0:{}", endpoint.port),
                service_id: endpoint.service.clone(),
                protocol: endpoint.protocol.clone(),
                expose: enabled_text(endpoint.expose),
                source: set.id.clone(),
            });
        }
    }
    rows.sort_by(|left, right| {
        left.service_id
            .cmp(&right.service_id)
            .then(left.endpoint.cmp(&right.endpoint))
    });
    rows
}

fn endpoint_models(manifests: &[(ServiceManifest, PathBuf)], sets: &[ServiceSet]) -> Vec<Endpoint> {
    let mut rows = manifests
        .iter()
        .map(|(manifest, _path)| Endpoint {
            endpoint: format!("127.0.0.1:{}", manifest.endpoint.default_port),
            service_id: manifest.id.clone(),
            protocol: manifest.endpoint.protocol.clone(),
            health_path: manifest.endpoint.health_path.clone(),
            health: "unknown".to_string(),
            reachable: false,
            display_name: manifest.name.clone(),
            note: "由 service.yaml 生成的本地预览 Endpoint".to_string(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect::<Vec<_>>();
    for set in sets {
        for endpoint in &set.default_endpoints {
            rows.push(Endpoint {
                endpoint: format!("127.0.0.1:{}", endpoint.port),
                service_id: endpoint.service.clone(),
                protocol: endpoint.protocol.clone(),
                health_path: String::new(),
                health: "unknown".to_string(),
                reachable: false,
                display_name: format!("{} default endpoint", endpoint.service),
                note: format!("由 Set {} 生成的本地预览 Endpoint", set.id),
                config: serde_json::json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            });
        }
    }
    rows.sort_by(|left, right| left.endpoint.cmp(&right.endpoint));
    rows.dedup_by(|left, right| left.endpoint == right.endpoint);
    rows
}

fn link_rows(sets: &[ServiceSet]) -> Vec<LinkViewRow> {
    let mut rows = Vec::new();
    for set in sets {
        for link in &set.default_links {
            let source_endpoint = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.from)
                .map(endpoint_from_set_endpoint)
                .unwrap_or_else(|| link.from.clone());
            let target_endpoint = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.to)
                .map(endpoint_from_set_endpoint)
                .unwrap_or_else(|| link.to.clone());
            rows.push(LinkViewRow {
                from: source_endpoint,
                to: target_endpoint,
                protocol: empty_to_default(&link.protocol, "runtime").to_string(),
                auth_mode: empty_to_default(&link.auth_mode, "internal").to_string(),
                scope: link.scope.clone(),
                source: set.id.clone(),
            });
        }
    }
    rows.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then(left.to.cmp(&right.to))
            .then(left.source.cmp(&right.source))
    });
    rows
}

fn link_models(sets: &[ServiceSet]) -> Vec<Link> {
    let mut rows = Vec::new();
    for set in sets {
        for link in &set.default_links {
            let source_endpoint = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.from)
                .map(|endpoint| format!("127.0.0.1:{}", endpoint.port));
            let target_endpoint = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.to)
                .map(|endpoint| format!("127.0.0.1:{}", endpoint.port));
            let (Some(source_endpoint), Some(target_endpoint)) = (source_endpoint, target_endpoint)
            else {
                continue;
            };
            rows.push(Link {
                source_endpoint,
                target_endpoint,
                protocol: empty_to_default(&link.protocol, "http").to_string(),
                auth_mode: empty_to_default(&link.auth_mode, "internal").to_string(),
                scope: link.scope.clone(),
                health: "unknown".to_string(),
                latency_ms: None,
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: serde_json::json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            });
        }
    }
    rows
}

fn topology_model(
    manifests: &[(ServiceManifest, PathBuf)],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    links: &[Link],
) -> Result<Topology> {
    let root_endpoint = endpoints
        .iter()
        .find(|endpoint| endpoint.service_id == "gateway")
        .or_else(|| endpoints.first())
        .map(|endpoint| endpoint.endpoint.clone())
        .ok_or_else(|| {
            OrchestratorError::Dependency("缺少可用于拓扑预览的 Endpoint".to_string())
        })?;
    build_topology(
        root_endpoint,
        manifests
            .iter()
            .map(|(manifest, _path)| manifest.id.clone())
            .collect(),
        sets.iter().map(|set| set.id.clone()).collect(),
        endpoints.to_vec(),
        links.to_vec(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

fn operation_rows(
    schemas: &SharedSchemas,
    manifests: &[(ServiceManifest, PathBuf)],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Vec<OperationViewRow> {
    let manifest_values = manifests
        .iter()
        .map(|(manifest, _path)| manifest.clone())
        .collect::<Vec<_>>();
    match validate_action_catalog(schemas) {
        Ok(descriptors) => descriptors
            .into_iter()
            .map(|descriptor| {
                let preview = default_action_request(descriptor.action).and_then(|request| {
                    plan_action_preview(&request, &manifest_values, sets, endpoints, topology).ok()
                });
                OperationViewRow {
                    operation_id: preview
                        .as_ref()
                        .map(|item| item.operation_id.clone())
                        .unwrap_or_default(),
                    action: descriptor.action.to_string(),
                    target: descriptor.target_type.to_string(),
                    status: "CATALOG".to_string(),
                    risk: descriptor.risk_label().to_string(),
                    plan_required: descriptor.plan_requirement().to_string(),
                    mode: descriptor.mode_label().to_string(),
                    fields: form_fields_text(schemas, descriptor.action),
                    preview_target: preview
                        .as_ref()
                        .map(|item| item.target_id.clone())
                        .unwrap_or_else(|| "待输入".to_string()),
                    preview_steps: preview
                        .as_ref()
                        .map(|item| {
                            if item.steps.is_empty() {
                                "无变更步骤".to_string()
                            } else {
                                item.steps.join(", ")
                            }
                        })
                        .unwrap_or_else(|| "待输入".to_string()),
                    preview_confirmation: preview
                        .as_ref()
                        .map(|item| enabled_text(item.requires_confirmation))
                        .unwrap_or_else(|| "待输入".to_string()),
                    result: String::new(),
                    error: String::new(),
                    log_count: 0,
                    summary: descriptor.summary.to_string(),
                    created_at: String::new(),
                    updated_at: String::new(),
                }
            })
            .collect(),
        Err(err) => vec![OperationViewRow {
            operation_id: String::new(),
            action: "action.catalog.invalid".to_string(),
            target: "DiagnosticReport".to_string(),
            status: "FAILED".to_string(),
            risk: "高".to_string(),
            plan_required: "必须修复".to_string(),
            mode: "阻塞".to_string(),
            fields: String::new(),
            preview_target: String::new(),
            preview_steps: String::new(),
            preview_confirmation: String::new(),
            result: String::new(),
            error: err.to_string(),
            log_count: 0,
            summary: err.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }],
    }
}

fn operation_store_rows(
    operations: &[Operation],
    log_counts: &HashMap<String, usize>,
) -> Vec<OperationViewRow> {
    operations
        .iter()
        .map(|operation| OperationViewRow {
            operation_id: operation.operation_id.clone(),
            action: operation.action.clone(),
            target: operation.target_type.clone(),
            status: operation_status_text(&operation.status),
            risk: String::new(),
            plan_required: operation_status_text(&operation.status),
            mode: "store".to_string(),
            fields: operation.target_id.clone(),
            preview_target: operation.target_id.clone(),
            preview_steps: operation
                .plan
                .get("steps")
                .and_then(serde_json::Value::as_array)
                .map(|steps| steps.len().to_string())
                .unwrap_or_else(|| "0".to_string()),
            preview_confirmation: operation
                .confirmed_at
                .is_empty()
                .then_some("no".to_string())
                .unwrap_or_else(|| "yes".to_string()),
            result: operation_result_summary(operation),
            error: operation.error_message.clone(),
            log_count: log_counts
                .get(&operation.operation_id)
                .copied()
                .unwrap_or_default(),
            summary: operation_summary(operation),
            created_at: operation.created_at.clone(),
            updated_at: operation.updated_at.clone(),
        })
        .collect()
}

fn log_model_row(log: &crate::LogView) -> LogViewRow {
    LogViewRow {
        source_id: log.source_id.clone(),
        service_id: log.service_id.clone(),
        endpoint: log.endpoint.clone(),
        operation_id: log.operation_id.clone(),
        level: "source".to_string(),
        message: log.display_name.clone(),
        path: log.path.clone(),
    }
}

fn operation_log_rows<S: OrchestratorStore>(
    store: &S,
    operations: &[Operation],
) -> Result<Vec<LogViewRow>> {
    let mut rows = Vec::new();
    for operation in operations {
        for record in store.operation_logs(&operation.operation_id)? {
            rows.push(operation_log_row(operation, &record));
        }
    }
    Ok(rows)
}

fn operation_log_row(operation: &Operation, record: &OperationLogRecord) -> LogViewRow {
    LogViewRow {
        source_id: format!("operation:{}", operation.operation_id),
        service_id: operation.target_id.clone(),
        endpoint: String::new(),
        operation_id: record.operation_id.clone(),
        level: record.level.clone(),
        message: record.message.clone(),
        path: if record.step_id.is_empty() {
            "operation".to_string()
        } else {
            format!("step:{}", record.step_id)
        },
    }
}

fn operation_log_counts(logs: &[LogViewRow]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for log in logs {
        if !log.operation_id.is_empty() {
            *counts.entry(log.operation_id.clone()).or_insert(0) += 1;
        }
    }
    counts
}

fn diagnostic_model_row(report: &crate::DiagnosticReport) -> DiagnosticViewRow {
    DiagnosticViewRow {
        target: format!("{} {}", report.target_type, report.target_id),
        status: report.status.clone(),
        summary: report.summary.clone(),
    }
}

fn operation_workbench_view(
    schemas: &SharedSchemas,
    manifests: &[(ServiceManifest, PathBuf)],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Option<OperationWorkbenchView> {
    let manifest_values = manifests
        .iter()
        .map(|(manifest, _path)| manifest.clone())
        .collect::<Vec<_>>();
    let workbench = build_operation_workbench(
        "service.install",
        schemas,
        &manifest_values,
        sets,
        endpoints,
        topology,
    )
    .ok()?;
    let session = new_operation_workbench_session(workbench.clone());
    let run = run_operation_workbench_flow(&workbench).ok();
    let mut view = operation_workbench_view_from_parts(&workbench, &session);
    view.result_status = run
        .as_ref()
        .map(|item| item.result_status.clone())
        .unwrap_or_default();
    view.log_count = run.as_ref().map(|item| item.logs.len()).unwrap_or_default();
    Some(view)
}

fn operation_workbench_view_from_parts(
    workbench: &OperationWorkbench,
    session: &OperationWorkbenchSession,
) -> OperationWorkbenchView {
    OperationWorkbenchView {
        selected_action: workbench.selected_action.clone(),
        operation_id: session.current_operation.operation_id.clone(),
        target: format!(
            "{} {}",
            workbench.preview.target_type, workbench.preview.target_id
        ),
        fields: workbench
            .form_fields
            .iter()
            .map(|field| {
                let suffix = if field.required { "*" } else { "" };
                format!("{}{}", field.name, suffix)
            })
            .collect::<Vec<_>>()
            .join(", "),
        editable_fields: workbench
            .form_fields
            .iter()
            .map(|field| OperationWorkbenchFieldView {
                name: field.name.clone(),
                field_type: field.field_type.clone(),
                required: field.required,
                value: workbench
                    .request
                    .fields
                    .get(&field.name)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect(),
        preview_steps: if workbench.preview.steps.is_empty() {
            "无变更步骤".to_string()
        } else {
            workbench.preview.steps.join(", ")
        },
        requires_confirmation: enabled_text(workbench.preview.requires_confirmation),
        can_apply: enabled_text(workbench.can_apply),
        rollback: enabled_text(workbench.can_rollback_after_success),
        current_status: format!("{:?}", session.current_operation.status),
        result_status: session.result_status.clone(),
        log_count: session.logs.len(),
        warnings: if workbench.warnings.is_empty() {
            "无".to_string()
        } else {
            workbench.warnings.join("；")
        },
    }
}

fn form_fields_text(schemas: &SharedSchemas, action: &str) -> String {
    schemas
        .form_for(action)
        .map(|form| {
            if form.fields.is_empty() {
                "无".to_string()
            } else {
                form.fields
                    .iter()
                    .map(|field| {
                        if field.required {
                            format!("{}*", field.name)
                        } else {
                            field.name.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        })
        .unwrap_or_else(|| "缺失".to_string())
}

fn log_rows(manifests: &[(ServiceManifest, PathBuf)]) -> Vec<LogViewRow> {
    manifests
        .iter()
        .map(|(manifest, _path)| LogViewRow {
            source_id: format!("{}:health", manifest.id),
            service_id: manifest.id.clone(),
            endpoint: endpoint_from_service(manifest),
            operation_id: String::new(),
            level: "source".to_string(),
            message: manifest.name.clone(),
            path: if manifest.endpoint.health_path.is_empty() {
                "metadata".to_string()
            } else {
                manifest.endpoint.health_path.clone()
            },
        })
        .collect()
}

fn diagnostic_rows(
    manifests: &[(ServiceManifest, PathBuf)],
    sets: &[ServiceSet],
    warnings: &[String],
) -> Vec<DiagnosticViewRow> {
    let service_ids = manifests
        .iter()
        .map(|(manifest, _path)| manifest.id.as_str())
        .collect::<HashSet<_>>();
    let mut rows = vec![DiagnosticViewRow {
        target: "service.yaml".to_string(),
        status: if warnings.is_empty() { "ok" } else { "warning" }.to_string(),
        summary: format!(
            "{} 个 Service，{} 条校验警告",
            manifests.len(),
            warnings.len()
        ),
    }];

    let mut missing = 0usize;
    for set in sets {
        for service in &set.services {
            if !service_ids.contains(service.id()) {
                missing += 1;
            }
        }
        for link in &set.default_links {
            if !service_ids.contains(link.from.as_str()) || !service_ids.contains(link.to.as_str())
            {
                missing += 1;
            }
        }
    }
    rows.push(DiagnosticViewRow {
        target: "set.yaml".to_string(),
        status: if missing == 0 { "ok" } else { "error" }.to_string(),
        summary: format!("{} 个 Set，缺失引用 {}", sets.len(), missing),
    });

    let worker_set_has_web_shell = sets.iter().any(|set| {
        set.id == "judge-worker-node"
            && set
                .services
                .iter()
                .any(|service| service.id() == "web-shell")
    });
    rows.push(DiagnosticViewRow {
        target: "judge-worker-node".to_string(),
        status: if worker_set_has_web_shell {
            "error"
        } else {
            "ok"
        }
        .to_string(),
        summary: "受限运行环境不包含 web-shell".to_string(),
    });
    rows
}

pub fn endpoint_hosts(endpoints: &[EndpointViewRow]) -> Vec<String> {
    let mut hosts = endpoints
        .iter()
        .filter_map(|endpoint| {
            endpoint
                .endpoint
                .rsplit_once(':')
                .map(|item| item.0.to_string())
        })
        .collect::<Vec<_>>();
    hosts.sort();
    hosts.dedup();
    hosts
}

fn endpoint_from_service(manifest: &ServiceManifest) -> String {
    let endpoint = format!("0.0.0.0:{}", manifest.endpoint.default_port);
    if validate_endpoint_id(&endpoint).is_ok() {
        endpoint
    } else {
        format!("invalid:{}", manifest.endpoint.default_port)
    }
}

fn endpoint_from_set_endpoint(endpoint: &crate::ServiceSetEndpoint) -> String {
    format!("0.0.0.0:{}", endpoint.port)
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn empty_to_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}

fn enabled_text(value: bool) -> String {
    if value {
        "是".to_string()
    } else {
        "否".to_string()
    }
}

fn operation_status_text(status: &crate::OperationStatus) -> String {
    match status {
        crate::OperationStatus::Planned => "PLANNED",
        crate::OperationStatus::AwaitingConfirmation => "AWAITING_CONFIRMATION",
        crate::OperationStatus::Running => "RUNNING",
        crate::OperationStatus::Succeeded => "SUCCEEDED",
        crate::OperationStatus::Failed => "FAILED",
        crate::OperationStatus::RolledBack => "ROLLED_BACK",
        crate::OperationStatus::Cancelled => "CANCELLED",
        crate::OperationStatus::Expired => "EXPIRED",
    }
    .to_string()
}

fn operation_result_summary(operation: &Operation) -> String {
    operation
        .result
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            if operation.result.is_null() {
                None
            } else {
                Some("result".to_string())
            }
        })
        .unwrap_or_default()
}

fn operation_summary(operation: &Operation) -> String {
    if !operation.error_message.is_empty() {
        return operation.error_message.clone();
    }
    let result = operation_result_summary(operation);
    if !result.is_empty() {
        return result;
    }
    format!("{} {}", operation.target_type, operation.target_id)
}

pub fn ensure_view_is_loaded(view: &OrchestratorView) -> Result<()> {
    if view.services.is_empty() {
        return Err(OrchestratorError::Dependency(
            "Orchestrator view has no services".to_string(),
        ));
    }
    if view.sets.is_empty() {
        return Err(OrchestratorError::Dependency(
            "Orchestrator view has no sets".to_string(),
        ));
    }
    Ok(())
}
