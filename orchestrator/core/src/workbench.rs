use crate::{
    ActionPlanPreview, ActionRequest, Endpoint, FormFieldSchema, MemoryOrchestratorStore,
    Operation, OperationExecutor, OperationLogRecord, OperationStatus, OrchestratorError,
    OrchestratorStore, PgOrchestratorStore, Result, ServiceManifest, ServiceSet, SharedSchemas,
    Topology, action_descriptor, build_topology, confirm_operation, default_action_request,
    plan_action_request, preview_operation, validate_service_manifest_file,
    validate_service_set_file,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbenchContext {
    pub schemas: SharedSchemas,
    pub services: Vec<ServiceManifest>,
    pub sets: Vec<ServiceSet>,
    pub endpoints: Vec<Endpoint>,
    pub links: Vec<crate::Link>,
    pub topology: Option<Topology>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(skip)]
    store_mode: WorkbenchStoreMode,
}

impl OperationWorkbenchContext {
    pub fn with_memory_store(mut self) -> Self {
        self.store_mode = WorkbenchStoreMode::Memory;
        self
    }

    pub fn uses_persistent_store(&self) -> bool {
        matches!(self.store_mode, WorkbenchStoreMode::PersistentFromEnv)
    }

    pub fn build_workbench(&self, action: &str) -> Result<OperationWorkbench> {
        build_operation_workbench(
            action,
            &self.schemas,
            &self.services,
            &self.sets,
            &self.endpoints,
            self.topology.as_ref(),
        )
    }

    pub fn build_session(&self, action: &str) -> Result<OperationWorkbenchSession> {
        Ok(new_operation_workbench_session(
            self.build_workbench(action)?,
        ))
    }

    pub fn build_session_from_request(
        &self,
        request: &ActionRequest,
    ) -> Result<OperationWorkbenchSession> {
        let workbench = build_operation_workbench_from_request(
            request,
            &self.schemas,
            &self.services,
            &self.sets,
            &self.endpoints,
            self.topology.as_ref(),
        )?;
        Ok(new_operation_workbench_session(workbench))
    }

    pub fn update_field(
        &self,
        session: &OperationWorkbenchSession,
        field: &str,
        value: impl Into<String>,
    ) -> Result<OperationWorkbenchSession> {
        update_operation_workbench_field(
            session,
            field,
            value,
            &self.schemas,
            &self.services,
            &self.sets,
            &self.endpoints,
            self.topology.as_ref(),
        )
    }

    pub fn suggested_field_values(&self, field: &FormFieldSchema) -> Vec<String> {
        let mut values = if field.values.is_empty() {
            match field.field_type.as_str() {
                "boolean" => vec!["true".to_string(), "false".to_string()],
                "endpoint" => self
                    .endpoints
                    .iter()
                    .map(|endpoint| endpoint.endpoint.clone())
                    .collect(),
                "service_id" => self
                    .services
                    .iter()
                    .map(|service| service.id.clone())
                    .collect(),
                "set_id" => self.sets.iter().map(|set| set.id.clone()).collect(),
                "secret_ref" => vec!["secret://example".to_string()],
                _ => match field.name.as_str() {
                    "source_endpoint" | "target_endpoint" => self
                        .endpoints
                        .iter()
                        .map(|endpoint| endpoint.endpoint.clone())
                        .collect(),
                    "protocol" => vec![
                        "http".to_string(),
                        "https".to_string(),
                        "tcp".to_string(),
                        "postgres".to_string(),
                        "redis".to_string(),
                    ],
                    "auth_mode" => vec!["internal".to_string(), "none".to_string()],
                    "format" => vec!["json".to_string(), "yaml".to_string()],
                    _ => Vec::new(),
                },
            }
        } else {
            field.values.clone()
        };
        values.sort();
        values.dedup();
        values
    }

    pub fn cycle_field_value(
        &self,
        session: &OperationWorkbenchSession,
        field_name: &str,
    ) -> Result<OperationWorkbenchSession> {
        let field = session
            .workbench
            .form_fields
            .iter()
            .find(|item| item.name == field_name)
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!("当前 action 不包含字段 {field_name}"))
            })?;
        let values = self.suggested_field_values(field);
        if values.is_empty() {
            return Err(OrchestratorError::Dependency(format!(
                "字段 {field_name} 没有可循环的候选值"
            )));
        }
        let current = session.workbench.request.field(field_name).unwrap_or("");
        let index = values
            .iter()
            .position(|value| value == current)
            .map(|index| (index + 1) % values.len())
            .unwrap_or(0);
        self.update_field(session, field_name, values[index].clone())
    }

    pub fn confirm(
        &self,
        session: &OperationWorkbenchSession,
    ) -> Result<OperationWorkbenchSession> {
        confirm_operation_workbench_session(session)
    }

    pub fn apply(&self, session: &OperationWorkbenchSession) -> Result<OperationWorkbenchSession> {
        match self.store_mode {
            WorkbenchStoreMode::Memory => {
                let mut store = self.session_store(session)?;
                apply_operation_workbench_session_with_store(session, &mut store)
            }
            WorkbenchStoreMode::PersistentFromEnv => {
                let mut store = self.persistent_session_store(session)?;
                apply_operation_workbench_session_with_store(session, &mut store)
            }
        }
    }

    pub fn rollback(
        &self,
        session: &OperationWorkbenchSession,
    ) -> Result<OperationWorkbenchSession> {
        match self.store_mode {
            WorkbenchStoreMode::Memory => {
                let mut store = self.session_store(session)?;
                rollback_operation_workbench_session_with_store(session, &mut store)
            }
            WorkbenchStoreMode::PersistentFromEnv => {
                let mut store = self.persistent_session_store(session)?;
                rollback_operation_workbench_session_with_store(session, &mut store)
            }
        }
    }

    fn session_store(
        &self,
        session: &OperationWorkbenchSession,
    ) -> Result<MemoryOrchestratorStore> {
        let mut store = MemoryOrchestratorStore::new();
        for service in &self.services {
            store.put_service(service.clone())?;
        }
        for set in &self.sets {
            store.put_set(set.clone())?;
        }
        for endpoint in &self.endpoints {
            store.put_endpoint(endpoint.clone())?;
        }
        for link in &self.links {
            store.put_link(link.clone())?;
        }
        if let Some(topology) = &self.topology {
            store.put_topology(topology.clone())?;
        }
        store.put_operation(session.current_operation.clone())?;
        for record in &session.logs {
            store.append_operation_log(record.clone())?;
        }
        Ok(store)
    }

    fn persistent_session_store(
        &self,
        session: &OperationWorkbenchSession,
    ) -> Result<PgOrchestratorStore> {
        let mut store = PgOrchestratorStore::from_env()?;
        seed_session_store(&mut store, self, session)?;
        Ok(store)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum WorkbenchStoreMode {
    #[default]
    Memory,
    PersistentFromEnv,
}

pub fn load_operation_workbench_context(repo_root: &Path) -> Result<OperationWorkbenchContext> {
    let mut warnings = Vec::new();
    let schemas = crate::load_shared_schemas(repo_root)?;
    let services = load_service_manifests(repo_root, &mut warnings)?;
    let sets = load_sets(repo_root, &mut warnings)?;
    let endpoints = endpoint_models(&services, &sets);
    let links = link_models(&sets);
    let topology = match topology_model(&services, &sets, &endpoints, &links) {
        Ok(topology) => Some(topology),
        Err(err) => {
            warnings.push(err.to_string());
            None
        }
    };

    Ok(OperationWorkbenchContext {
        schemas,
        services,
        sets,
        endpoints,
        links,
        topology,
        warnings,
        store_mode: if std::env::var(PgOrchestratorStore::ENV_NAME).is_ok() {
            WorkbenchStoreMode::PersistentFromEnv
        } else {
            WorkbenchStoreMode::Memory
        },
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbench {
    pub selected_action: String,
    pub request: ActionRequest,
    pub form_fields: Vec<FormFieldSchema>,
    pub required_fields_satisfied: bool,
    pub preview: ActionPlanPreview,
    pub operation: Operation,
    pub can_confirm: bool,
    pub can_apply: bool,
    pub can_rollback_after_success: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbenchRun {
    pub operation_id: String,
    pub action: String,
    pub planned_status: OperationStatus,
    pub confirmed_status: Option<OperationStatus>,
    pub applied_status: OperationStatus,
    pub rolled_back_status: Option<OperationStatus>,
    pub result_status: String,
    pub logs: Vec<OperationLogRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbenchSession {
    pub workbench: OperationWorkbench,
    pub current_operation: Operation,
    #[serde(default)]
    pub result_status: String,
    #[serde(default)]
    pub logs: Vec<OperationLogRecord>,
}

pub fn build_operation_workbench(
    action: &str,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbench> {
    let request = default_action_request(action).ok_or_else(|| {
        OrchestratorError::Dependency(format!("action {action} has no default request"))
    })?;
    build_operation_workbench_from_request(&request, schemas, services, sets, endpoints, topology)
}

pub fn build_operation_workbench_from_request(
    request: &ActionRequest,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbench> {
    let action = request.action.as_str();
    let descriptor = action_descriptor(action)
        .ok_or_else(|| OrchestratorError::InvalidManifest(format!("unknown action {action}")))?;
    let form = schemas
        .form_for(action)
        .ok_or_else(|| OrchestratorError::Dependency(format!("action {action} has no form")))?;
    let operation = plan_action_request(request, services, sets, endpoints, topology)?;
    let preview = preview_operation(&operation, descriptor);
    let required_fields_satisfied = form
        .fields
        .iter()
        .filter(|field| field.required)
        .all(|field| request.field(&field.name).is_some());
    let mut warnings = Vec::new();
    if !required_fields_satisfied {
        warnings.push("必填字段尚未完整填写".to_string());
    }

    Ok(OperationWorkbench {
        selected_action: action.to_string(),
        request: request.clone(),
        form_fields: form.fields.clone(),
        required_fields_satisfied,
        can_confirm: preview.requires_confirmation,
        can_apply: required_fields_satisfied,
        can_rollback_after_success: preview.rollback_available,
        preview,
        operation,
        warnings,
    })
}

pub fn new_operation_workbench_session(workbench: OperationWorkbench) -> OperationWorkbenchSession {
    OperationWorkbenchSession {
        current_operation: workbench.operation.clone(),
        workbench,
        result_status: String::new(),
        logs: Vec::new(),
    }
}

pub fn update_operation_workbench_field(
    session: &OperationWorkbenchSession,
    field: &str,
    value: impl Into<String>,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbenchSession> {
    let mut request = session.workbench.request.clone();
    request.fields.insert(field.to_string(), value.into());
    let workbench = build_operation_workbench_from_request(
        &request, schemas, services, sets, endpoints, topology,
    )?;
    Ok(new_operation_workbench_session(workbench))
}

pub fn confirm_operation_workbench_session(
    session: &OperationWorkbenchSession,
) -> Result<OperationWorkbenchSession> {
    let confirmed = confirm_operation(&session.current_operation)?;
    let mut next = session.clone();
    next.current_operation = confirmed;
    next.logs = session.logs.clone();
    next.result_status.clear();
    Ok(next)
}

pub fn apply_operation_workbench_session(
    session: &OperationWorkbenchSession,
) -> Result<OperationWorkbenchSession> {
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(session.current_operation.clone())?;
    for record in &session.logs {
        store.append_operation_log(record.clone())?;
    }
    apply_operation_workbench_session_with_store(session, &mut store)
}

fn apply_operation_workbench_session_with_store(
    session: &OperationWorkbenchSession,
    store: &mut impl OrchestratorStore,
) -> Result<OperationWorkbenchSession> {
    let applied = OperationExecutor::new(store).apply(&session.current_operation.operation_id)?;
    let result_status = applied
        .result
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut next = session.clone();
    next.current_operation = applied;
    next.logs = store.operation_logs(&session.current_operation.operation_id)?;
    next.result_status = result_status;
    Ok(next)
}

pub fn rollback_operation_workbench_session(
    session: &OperationWorkbenchSession,
) -> Result<OperationWorkbenchSession> {
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(session.current_operation.clone())?;
    for record in &session.logs {
        store.append_operation_log(record.clone())?;
    }
    rollback_operation_workbench_session_with_store(session, &mut store)
}

fn rollback_operation_workbench_session_with_store(
    session: &OperationWorkbenchSession,
    store: &mut impl OrchestratorStore,
) -> Result<OperationWorkbenchSession> {
    let rolled_back =
        OperationExecutor::new(store).rollback(&session.current_operation.operation_id)?;
    let result_status = rolled_back
        .result
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut next = session.clone();
    next.current_operation = rolled_back;
    next.logs = store.operation_logs(&session.current_operation.operation_id)?;
    next.result_status = result_status;
    Ok(next)
}

fn seed_session_store(
    store: &mut impl OrchestratorStore,
    context: &OperationWorkbenchContext,
    session: &OperationWorkbenchSession,
) -> Result<()> {
    for service in &context.services {
        store.put_service(service.clone())?;
    }
    for set in &context.sets {
        store.put_set(set.clone())?;
    }
    for endpoint in &context.endpoints {
        store.put_endpoint(endpoint.clone())?;
    }
    for link in &context.links {
        store.put_link(link.clone())?;
    }
    if let Some(topology) = &context.topology {
        store.put_topology(topology.clone())?;
    }
    store.put_operation(session.current_operation.clone())?;
    for record in &session.logs {
        store.append_operation_log(record.clone())?;
    }
    Ok(())
}

pub fn run_operation_workbench_flow(
    workbench: &OperationWorkbench,
) -> Result<OperationWorkbenchRun> {
    if !workbench.required_fields_satisfied {
        return Err(OrchestratorError::Blocked(
            "operation form required fields are incomplete".to_string(),
        ));
    }

    let mut session = new_operation_workbench_session(workbench.clone());
    let confirmed_status = if workbench.preview.requires_confirmation {
        session = confirm_operation_workbench_session(&session)?;
        Some(session.current_operation.status.clone())
    } else {
        None
    };
    session = apply_operation_workbench_session(&session)?;
    let applied_status = session.current_operation.status.clone();
    let result_status = session.result_status.clone();
    let rolled_back_status = if workbench.can_rollback_after_success {
        session = rollback_operation_workbench_session(&session)?;
        Some(session.current_operation.status.clone())
    } else {
        None
    };

    Ok(OperationWorkbenchRun {
        operation_id: workbench.operation.operation_id.clone(),
        action: workbench.operation.action.clone(),
        planned_status: OperationStatus::Planned,
        confirmed_status,
        applied_status,
        rolled_back_status,
        result_status,
        logs: session.logs,
    })
}

fn load_service_manifests(
    repo_root: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<ServiceManifest>> {
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
            Ok(manifest) => rows.push(manifest),
            Err(err) => warnings.push(format!("{}: {}", slash_path(&rel), err)),
        }
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
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

fn endpoint_models(services: &[ServiceManifest], sets: &[ServiceSet]) -> Vec<Endpoint> {
    let mut rows = services
        .iter()
        .map(|manifest| Endpoint {
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

fn link_models(sets: &[ServiceSet]) -> Vec<crate::Link> {
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
            rows.push(crate::Link {
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
    services: &[ServiceManifest],
    sets: &[ServiceSet],
    endpoints: &[Endpoint],
    links: &[crate::Link],
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
        services
            .iter()
            .map(|manifest| manifest.id.clone())
            .collect(),
        sets.iter().map(|set| set.id.clone()).collect(),
        endpoints.to_vec(),
        links.to_vec(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

fn slash_path(path: &PathBuf) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn empty_to_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}
