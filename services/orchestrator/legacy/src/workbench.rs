use crate::store::service_api_surfaces_from_release;
use crate::{
    ActionPlanPreview, ActionRequest, DeploymentTemplate, Endpoint, FormFieldSchema,
    MemoryOrchestratorStore, Operation, OperationExecutor, OperationLogRecord, OperationStatus,
    OrchestratorError, OrchestratorStore, PgOrchestratorStore, Result, ServiceManifest,
    ServiceRelease, ServiceReleaseManifest, SharedSchemas, Topology, action_descriptor,
    build_topology, confirm_operation, default_action_request, plan_action_request_with_releases,
    preview_operation, validate_deployment_template_file, validate_service_manifest_file,
    validate_service_release_file,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationWorkbenchContext {
    pub schemas: SharedSchemas,
    pub services: Vec<ServiceManifest>,
    #[serde(default)]
    pub releases: Vec<ServiceReleaseManifest>,
    pub templates: Vec<DeploymentTemplate>,
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

    fn with_persistent_store(mut self, database_url: String) -> Self {
        self.store_mode = WorkbenchStoreMode::Persistent { database_url };
        self
    }

    pub fn uses_persistent_store(&self) -> bool {
        matches!(self.store_mode, WorkbenchStoreMode::Persistent { .. })
    }

    pub fn persistent_database_url(&self) -> Option<&str> {
        match &self.store_mode {
            WorkbenchStoreMode::Memory => None,
            WorkbenchStoreMode::Persistent { database_url } => Some(database_url.as_str()),
        }
    }

    pub fn build_workbench(&self, action: &str) -> Result<OperationWorkbench> {
        build_operation_workbench_with_releases(
            action,
            &self.schemas,
            &self.services,
            &self.releases,
            &self.templates,
            &self.endpoints,
            self.topology.as_ref(),
        )
    }

    pub fn build_session(&self, action: &str) -> Result<OperationWorkbenchSession> {
        self.persist_planned_session(new_operation_workbench_session(
            self.build_workbench(action)?,
        ))
    }

    pub fn build_session_from_request(
        &self,
        request: &ActionRequest,
    ) -> Result<OperationWorkbenchSession> {
        let workbench = build_operation_workbench_from_request_with_releases(
            request,
            &self.schemas,
            &self.services,
            &self.releases,
            &self.templates,
            &self.endpoints,
            self.topology.as_ref(),
        )?;
        self.persist_planned_session(new_operation_workbench_session(workbench))
    }

    pub fn update_field(
        &self,
        session: &OperationWorkbenchSession,
        field: &str,
        value: impl Into<String>,
    ) -> Result<OperationWorkbenchSession> {
        let session = update_operation_workbench_field_with_releases(
            session,
            field,
            value,
            &self.schemas,
            &self.services,
            &self.releases,
            &self.templates,
            &self.endpoints,
            self.topology.as_ref(),
        )?;
        self.persist_planned_session(session)
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
                        "postgresql".to_string(),
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
        let confirmed = confirm_operation_workbench_session(session)?;
        self.persist_confirmed_session(confirmed)
    }

    pub fn apply(&self, session: &OperationWorkbenchSession) -> Result<OperationWorkbenchSession> {
        match self.store_mode {
            WorkbenchStoreMode::Memory => {
                let mut store = self.session_store(session)?;
                apply_operation_workbench_session_with_store(session, &mut store)
            }
            WorkbenchStoreMode::Persistent { .. } => {
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
            WorkbenchStoreMode::Persistent { .. } => {
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
        seed_session_store(&mut store, self, session)?;
        Ok(store)
    }

    fn persistent_session_store(
        &self,
        session: &OperationWorkbenchSession,
    ) -> Result<PgOrchestratorStore> {
        let database_url = self.persistent_database_url().ok_or_else(|| {
            OrchestratorError::Dependency("persistent store URL is not configured".to_string())
        })?;
        let mut store = PgOrchestratorStore::new(database_url.to_string())?;
        seed_session_store(&mut store, self, session)?;
        Ok(store)
    }

    fn persist_planned_session(
        &self,
        session: OperationWorkbenchSession,
    ) -> Result<OperationWorkbenchSession> {
        if self.uses_persistent_store() {
            self.persistent_session_store(&session)?;
        }
        Ok(session)
    }

    fn persist_confirmed_session(
        &self,
        session: OperationWorkbenchSession,
    ) -> Result<OperationWorkbenchSession> {
        if self.uses_persistent_store() {
            self.persistent_session_store(&session)?;
        }
        Ok(session)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum WorkbenchStoreMode {
    #[default]
    Memory,
    Persistent {
        database_url: String,
    },
}

pub fn load_operation_workbench_context(repo_root: &Path) -> Result<OperationWorkbenchContext> {
    load_operation_workbench_context_with_database_url(
        repo_root,
        std::env::var(PgOrchestratorStore::ENV_NAME).ok(),
    )
}

pub(crate) fn load_operation_workbench_context_with_database_url(
    repo_root: &Path,
    database_url: Option<String>,
) -> Result<OperationWorkbenchContext> {
    let schemas = crate::load_shared_schemas(repo_root)?;
    if let Some(database_url) = database_url {
        return PgOrchestratorStore::new(database_url.clone())
            .and_then(|store| {
                load_operation_workbench_context_from_store(schemas.clone(), &store)
                    .map(|context| context.with_persistent_store(database_url.clone()))
            })
            .map_err(|err| {
                OrchestratorError::Dependency(format!(
                    "ORCHESTRATOR_DATABASE_URL store unavailable: {err}"
                ))
            });
    }
    load_operation_workbench_context_from_repo(repo_root)
}

pub(crate) fn load_operation_workbench_context_from_repo(
    repo_root: &Path,
) -> Result<OperationWorkbenchContext> {
    let mut warnings = Vec::new();
    let schemas = crate::load_shared_schemas(repo_root)?;
    let services = load_service_manifests(repo_root, &mut warnings)?;
    let releases = load_service_releases(repo_root, &mut warnings)?;
    let templates = load_templates(repo_root, &mut warnings)?;
    let endpoints = endpoint_models(&services, &templates);
    let links = link_models(&templates);
    let topology = match topology_model(&services, &templates, &endpoints, &links) {
        Ok(topology) => Some(topology),
        Err(err) => {
            warnings.push(err.to_string());
            None
        }
    };

    Ok(OperationWorkbenchContext {
        schemas,
        services,
        releases,
        templates,
        endpoints,
        links,
        topology,
        warnings,
        store_mode: WorkbenchStoreMode::Memory,
    })
}

pub fn load_operation_workbench_context_from_store<S: OrchestratorStore>(
    schemas: SharedSchemas,
    store: &S,
) -> Result<OperationWorkbenchContext> {
    let services = store.list_services()?;
    let releases = store
        .list_service_releases()?
        .into_iter()
        .map(|record| serde_json::from_value(record.manifest).map_err(OrchestratorError::Json))
        .collect::<Result<Vec<ServiceReleaseManifest>>>()?;
    let templates = Vec::new();
    let endpoints = store.list_endpoints()?;
    let links = store.list_links()?;
    let topology = store
        .get_latest_topology_snapshot()?
        .map(|snapshot| snapshot.topology)
        .or_else(|| topology_model(&services, &templates, &endpoints, &links).ok());
    Ok(OperationWorkbenchContext {
        schemas,
        services,
        releases,
        templates,
        endpoints,
        links,
        topology,
        warnings: Vec::new(),
        store_mode: WorkbenchStoreMode::Memory,
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
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbench> {
    build_operation_workbench_with_releases(
        action,
        schemas,
        services,
        &[],
        sets,
        endpoints,
        topology,
    )
}

pub fn build_operation_workbench_with_releases(
    action: &str,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    releases: &[ServiceReleaseManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbench> {
    let request = default_action_request(action).ok_or_else(|| {
        OrchestratorError::Dependency(format!("action {action} has no default request"))
    })?;
    build_operation_workbench_from_request_with_releases(
        &request, schemas, services, releases, sets, endpoints, topology,
    )
}

pub fn build_operation_workbench_from_request(
    request: &ActionRequest,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbench> {
    build_operation_workbench_from_request_with_releases(
        request,
        schemas,
        services,
        &[],
        sets,
        endpoints,
        topology,
    )
}

pub fn build_operation_workbench_from_request_with_releases(
    request: &ActionRequest,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    releases: &[ServiceReleaseManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbench> {
    let action = request.action.as_str();
    let descriptor = action_descriptor(action)
        .ok_or_else(|| OrchestratorError::InvalidManifest(format!("unknown action {action}")))?;
    let form = schemas
        .form_for(action)
        .ok_or_else(|| OrchestratorError::Dependency(format!("action {action} has no form")))?;
    let operation =
        plan_action_request_with_releases(request, services, releases, sets, endpoints, topology)?;
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

// This public helper keeps the workbench's independent registry inputs explicit.
#[allow(clippy::too_many_arguments)]
pub fn update_operation_workbench_field(
    session: &OperationWorkbenchSession,
    field: &str,
    value: impl Into<String>,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbenchSession> {
    update_operation_workbench_field_with_releases(
        session,
        field,
        value,
        schemas,
        services,
        &[],
        sets,
        endpoints,
        topology,
    )
}

// This public helper keeps the workbench's independent registry inputs explicit.
#[allow(clippy::too_many_arguments)]
pub fn update_operation_workbench_field_with_releases(
    session: &OperationWorkbenchSession,
    field: &str,
    value: impl Into<String>,
    schemas: &SharedSchemas,
    services: &[ServiceManifest],
    releases: &[ServiceReleaseManifest],
    sets: &[DeploymentTemplate],
    endpoints: &[Endpoint],
    topology: Option<&Topology>,
) -> Result<OperationWorkbenchSession> {
    let mut request = session.workbench.request.clone();
    request.fields.insert(field.to_string(), value.into());
    let workbench = build_operation_workbench_from_request_with_releases(
        &request, schemas, services, releases, sets, endpoints, topology,
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

pub(crate) fn seed_session_store(
    store: &mut impl OrchestratorStore,
    context: &OperationWorkbenchContext,
    session: &OperationWorkbenchSession,
) -> Result<()> {
    for service in &context.services {
        store.put_service(service.clone())?;
    }
    for release in &context.releases {
        store.upsert_service_release(service_release_record(release)?)?;
        for api in service_api_surfaces_from_release(release)? {
            store.upsert_service_api_surface(api)?;
        }
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
    for entry in fs::read_dir(&services_dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if !entry.file_type().map_err(io_error)?.is_dir() {
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
            Err(err) => warnings.push(format!("{}: {}", slash_path(&rel)?, err)),
        }
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(rows)
}

fn load_service_releases(
    repo_root: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<ServiceReleaseManifest>> {
    let mut rows = Vec::new();
    let services_dir = repo_root.join("services");
    if !services_dir.is_dir() {
        return Ok(rows);
    }
    for entry in fs::read_dir(&services_dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if !entry.file_type().map_err(io_error)?.is_dir() {
            continue;
        }
        let rel = Path::new("services")
            .join(entry.file_name())
            .join("release.yaml");
        if !repo_root.join(&rel).is_file() {
            continue;
        }
        match validate_service_release_file(repo_root, &rel) {
            Ok(release) => rows.push(release),
            Err(err) => warnings.push(format!("{}: {}", slash_path(&rel)?, err)),
        }
    }
    rows.sort_by(|left, right| left.service_name.cmp(&right.service_name));
    Ok(rows)
}

fn load_templates(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<DeploymentTemplate>> {
    let mut templates = Vec::new();
    let sets_dir = repo_root.join("sets");
    if !sets_dir.is_dir() {
        warnings.push("sets/ 目录不存在".to_string());
        return Ok(templates);
    }
    for entry in fs::read_dir(&sets_dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("yaml") {
            continue;
        }
        let rel = Path::new("sets").join(entry.file_name());
        match validate_deployment_template_file(repo_root, &rel) {
            Ok(template) => templates.push(template),
            Err(err) => warnings.push(format!("{}: {}", slash_path(&rel)?, err)),
        }
    }
    templates.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(templates)
}

fn endpoint_models(services: &[ServiceManifest], sets: &[DeploymentTemplate]) -> Vec<Endpoint> {
    let mut rows = services
        .iter()
        .map(|manifest| Endpoint {
            endpoint: endpoint_id("127.0.0.1", manifest.endpoint.default_port, &manifest.id),
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
                endpoint: endpoint_id("127.0.0.1", endpoint.port, &endpoint.service),
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

fn link_models(sets: &[DeploymentTemplate]) -> Vec<crate::Link> {
    let mut rows = Vec::new();
    for set in sets {
        for link in &set.default_links {
            let source_endpoint = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.from)
                .map(|endpoint| endpoint_id("127.0.0.1", endpoint.port, &endpoint.service));
            let target_endpoint = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.to)
                .map(|endpoint| endpoint_id("127.0.0.1", endpoint.port, &endpoint.service));
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
                enabled: true,
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
    _sets: &[DeploymentTemplate],
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
        endpoints.to_vec(),
        links.to_vec(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

fn slash_path(path: &Path) -> Result<String> {
    Ok(path
        .to_str()
        .ok_or_else(|| OrchestratorError::UnsafePath("workbench path must be UTF-8".to_string()))?
        .replace('\\', "/"))
}

fn empty_to_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}

fn endpoint_id(host: &str, port: u16, service_id: &str) -> String {
    format!("{host}:{port}:{service_id}")
}

fn io_error(error: std::io::Error) -> OrchestratorError {
    OrchestratorError::Dependency(format!("workbench repository I/O failed: {}", error.kind()))
}

fn service_release_record(release: &ServiceReleaseManifest) -> Result<ServiceRelease> {
    Ok(ServiceRelease {
        service_name: release.service_name.clone(),
        version: release.version.clone(),
        release_url: release.source.url.clone(),
        manifest: serde_json::to_value(release)?,
        checksum: release.source.checksum.clone(),
        created_at: String::new(),
    })
}
