use crate::{
    ActionRequest, DiagnosticExport, DiagnosticReport, EndpointProbe, MemoryOrchestratorStore,
    Operation, OperationExecutor, OperationLogRecord, OperationStatus, OrchestratorError,
    OrchestratorStore, PgOrchestratorStore, Result, StaticEndpointProbe, TcpEndpointProbe,
    Topology, action_catalog, action_descriptor, cancel_operation, confirm_operation,
    default_action_request, export_diagnostic_report, load_operation_workbench_context,
    load_operation_workbench_context_from_store, load_orchestrator_view_from_store,
    operation_log_record, operation_step_log_record, plan_action_request, plan_operation,
};
use crate::{OperationWorkbenchContext, OrchestratorView, SharedSchemas};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionCapabilityStatus {
    Real,
    StoreBacked,
    Unsupported,
    Readonly,
}

impl ActionCapabilityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Real => "REAL",
            Self::StoreBacked => "STORE_BACKED",
            Self::Unsupported => "UNSUPPORTED",
            Self::Readonly => "READONLY",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionMatrixEntry {
    pub action_id: String,
    pub gui_entry: bool,
    pub tui_entry: bool,
    pub writes_store: bool,
    pub creates_operation: bool,
    pub requires_confirmation: bool,
    pub calls_executor: bool,
    pub capability_status: ActionCapabilityStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionDispatchResult {
    pub action_id: String,
    pub status: String,
    pub message: String,
    pub operation_id: String,
    pub result: Value,
    pub error: String,
    pub warnings: Vec<String>,
    pub changed_objects: Vec<String>,
    pub capability_status: ActionCapabilityStatus,
    pub logs: Vec<OperationLogRecord>,
}

pub struct OrchestratorActionDispatcher<
    'a,
    S: OrchestratorStore,
    P: EndpointProbe = TcpEndpointProbe,
> {
    store: &'a mut S,
    endpoint_probe: P,
}

impl<'a, S: OrchestratorStore> OrchestratorActionDispatcher<'a, S, TcpEndpointProbe> {
    pub fn new(store: &'a mut S) -> Self {
        Self {
            store,
            endpoint_probe: TcpEndpointProbe::new(Duration::from_millis(800)),
        }
    }
}

impl<'a, S: OrchestratorStore, P: EndpointProbe + Clone> OrchestratorActionDispatcher<'a, S, P> {
    pub fn with_endpoint_probe(store: &'a mut S, endpoint_probe: P) -> Self {
        Self {
            store,
            endpoint_probe,
        }
    }

    pub fn dispatch(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        action_descriptor(&request.action).ok_or_else(|| {
            OrchestratorError::InvalidManifest(format!("unknown action {}", request.action))
        })?;
        if matches!(
            capability_for_action(&request.action),
            ActionCapabilityStatus::Unsupported
        ) {
            let reason = unsupported_reason(&request.action);
            return self.dispatch_unsupported(request, reason);
        }
        match request.action.as_str() {
            "operation.confirm" => self.dispatch_operation_confirm(&request),
            "operation.apply" => self.dispatch_operation_apply(&request),
            "operation.cancel" => self.dispatch_operation_cancel(&request),
            "operation.rollback" => self.dispatch_operation_rollback(&request),
            "log.query" => self.dispatch_operation_logs(&request),
            _ => self.dispatch_planned_action(request),
        }
    }

    fn dispatch_planned_action(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        if matches!(
            capability_for_action(&request.action),
            ActionCapabilityStatus::Readonly
        ) {
            return self.dispatch_readonly_action(request);
        }

        let request = self.with_operation_id(request)?;
        let services = self.store.list_services()?;
        let releases = self
            .store
            .list_service_releases()?
            .into_iter()
            .map(|record| serde_json::from_value(record.manifest).map_err(OrchestratorError::Json))
            .collect::<Result<Vec<_>>>()?;
        let endpoints = self.store.list_endpoints()?;
        let topology = self
            .store
            .get_latest_topology_snapshot()?
            .map(|item| item.topology);
        let operation = crate::plan_action_request_with_releases(
            &request,
            &services,
            &releases,
            &[],
            &endpoints,
            topology.as_ref(),
        )?;
        self.store.update_operation(operation.clone())?;
        self.store.append_operation_log(operation_log_record(
            &operation.operation_id,
            "info",
            format!("action {} planned by GUI/TUI console", operation.action),
        ))?;

        let requires_confirmation = operation
            .plan
            .get("requires_confirmation")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                action_descriptor(&operation.action)
                    .is_some_and(|descriptor| descriptor.plan_mode.requires_confirmation())
            });
        let confirmed = request.field("confirm") == Some("true");
        if requires_confirmation && !confirmed {
            return self.result_for_operation(
                &operation,
                "PLANNED",
                "Operation 已生成计划，等待确认后执行",
                capability_for_action(&operation.action),
                Vec::new(),
            );
        }

        let operation = if requires_confirmation {
            let confirmed_operation = confirm_operation(&operation)?;
            self.store.update_operation(confirmed_operation.clone())?;
            self.store.append_operation_log(operation_log_record(
                &confirmed_operation.operation_id,
                "info",
                "operation confirmed",
            ))?;
            confirmed_operation
        } else {
            operation
        };

        let mut executor =
            OperationExecutor::with_endpoint_probe(self.store, self.endpoint_probe.clone());
        match executor.apply(&operation.operation_id) {
            Ok(applied) => self.result_for_operation(
                &applied,
                operation_result_status(&applied),
                "Action 已通过 core dispatcher 执行",
                capability_for_action(&applied.action),
                changed_objects_from_result(&applied.result),
            ),
            Err(err) => {
                let operation = self
                    .store
                    .get_operation(&operation.operation_id)?
                    .unwrap_or(operation);
                let capability = if is_service_lifecycle_action(&operation.action) {
                    ActionCapabilityStatus::Unsupported
                } else {
                    capability_for_action(&operation.action)
                };
                self.result_for_operation(
                    &operation,
                    "FAILED",
                    "Action 执行失败",
                    capability,
                    Vec::new(),
                )
                .map(|mut result| {
                    result.error = err.to_string();
                    result.message = format!("{}: {}", result.message, result.error);
                    result
                })
            }
        }
    }

    fn dispatch_readonly_action(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        let request = self.with_operation_id(request)?;
        let services = self.store.list_services()?;
        let endpoints = self.store.list_endpoints()?;
        let topology = self
            .store
            .get_latest_topology_snapshot()?
            .map(|item| item.topology);
        let operation =
            plan_action_request(&request, &services, &[], &endpoints, topology.as_ref())?;
        let result = readonly_result_for_action(self.store, &request)?;
        let mut traced = operation.clone();
        traced.result = result;
        self.store.update_operation(traced.clone())?;
        self.store.append_operation_log(operation_log_record(
            &traced.operation_id,
            "info",
            format!("readonly action {} evaluated", traced.action),
        ))?;
        self.result_for_operation(
            &traced,
            "READONLY",
            "只读 action 已完成，不改变 Service/Endpoint/Link/Topology",
            ActionCapabilityStatus::Readonly,
            Vec::new(),
        )
    }

    fn dispatch_unsupported(
        &mut self,
        request: ActionRequest,
        reason: &str,
    ) -> Result<ActionDispatchResult> {
        let request = self.with_operation_id(request)?;
        let descriptor = action_descriptor(&request.action).ok_or_else(|| {
            OrchestratorError::InvalidManifest(format!("unknown action {}", request.action))
        })?;
        let target_id = action_target_id(&request);
        let mut operation = plan_operation(
            &request.operation_id,
            &request.action,
            descriptor.target_type,
            &target_id,
            serde_json::json!(request.fields),
            serde_json::json!({
                "steps": [
                    {
                        "action": "unsupported",
                        "target": target_id,
                        "detail": reason
                    }
                ],
                "requires_confirmation": false
            }),
            serde_json::json!({
                "steps": []
            }),
        )?;
        operation.status = OperationStatus::Failed;
        operation.result = serde_json::json!({
            "status": "UNSUPPORTED",
            "reason": reason,
        });
        operation.error_message = reason.to_string();
        self.store.update_operation(operation.clone())?;
        self.store.append_operation_log(operation_step_log_record(
            &operation.operation_id,
            "unsupported",
            "warn",
            reason,
            operation.result.clone(),
        ))?;
        self.result_for_operation(
            &operation,
            "UNSUPPORTED",
            reason,
            ActionCapabilityStatus::Unsupported,
            Vec::new(),
        )
    }

    fn dispatch_operation_confirm(
        &mut self,
        request: &ActionRequest,
    ) -> Result<ActionDispatchResult> {
        let operation_id = request.require_field("operation_id")?;
        let operation = self.store.get_operation(operation_id)?.ok_or_else(|| {
            OrchestratorError::Dependency(format!("operation {operation_id} not found"))
        })?;
        let confirmed = confirm_operation(&operation)?;
        self.store.update_operation(confirmed.clone())?;
        self.store.append_operation_log(operation_log_record(
            &confirmed.operation_id,
            "info",
            "operation confirmed by GUI/TUI console",
        ))?;
        self.result_for_operation(
            &confirmed,
            "AWAITING_CONFIRMATION",
            "Operation 已确认",
            ActionCapabilityStatus::StoreBacked,
            Vec::new(),
        )
    }

    fn dispatch_operation_apply(
        &mut self,
        request: &ActionRequest,
    ) -> Result<ActionDispatchResult> {
        let operation_id = request.require_field("operation_id")?;
        let mut executor =
            OperationExecutor::with_endpoint_probe(self.store, self.endpoint_probe.clone());
        if request.field("execute_service_driver") == Some("true") {
            executor = executor.with_service_driver_execution_enabled();
        }
        match executor.apply(operation_id) {
            Ok(operation) => self.result_for_operation(
                &operation,
                operation_result_status(&operation),
                "Operation 已执行",
                capability_for_action(&operation.action),
                changed_objects_from_result(&operation.result),
            ),
            Err(err) => {
                let operation = self.store.get_operation(operation_id)?.ok_or_else(|| {
                    OrchestratorError::Dependency(format!("operation {operation_id} not found"))
                })?;
                self.result_for_operation(
                    &operation,
                    "FAILED",
                    "Operation 执行失败",
                    capability_for_action(&operation.action),
                    Vec::new(),
                )
                .map(|mut result| {
                    result.error = err.to_string();
                    result.message = format!("{}: {}", result.message, result.error);
                    result
                })
            }
        }
    }

    fn dispatch_operation_cancel(
        &mut self,
        request: &ActionRequest,
    ) -> Result<ActionDispatchResult> {
        let operation_id = request.require_field("operation_id")?;
        let operation = self.store.get_operation(operation_id)?.ok_or_else(|| {
            OrchestratorError::Dependency(format!("operation {operation_id} not found"))
        })?;
        let cancelled = cancel_operation(&operation)?;
        self.store.update_operation(cancelled.clone())?;
        self.store.append_operation_log(operation_log_record(
            &cancelled.operation_id,
            "info",
            "operation cancelled by GUI/TUI console",
        ))?;
        self.result_for_operation(
            &cancelled,
            "CANCELLED",
            "Operation cancelled",
            ActionCapabilityStatus::StoreBacked,
            Vec::new(),
        )
    }

    fn dispatch_operation_rollback(
        &mut self,
        request: &ActionRequest,
    ) -> Result<ActionDispatchResult> {
        let operation_id = request.require_field("operation_id")?;
        let mut executor = OperationExecutor::new(self.store);
        match executor.rollback(operation_id) {
            Ok(operation) => self.result_for_operation(
                &operation,
                "ROLLED_BACK",
                "Operation 已回滚",
                ActionCapabilityStatus::StoreBacked,
                changed_objects_from_result(&operation.result),
            ),
            Err(err) => {
                let operation = self.store.get_operation(operation_id)?.ok_or_else(|| {
                    OrchestratorError::Dependency(format!("operation {operation_id} not found"))
                })?;
                self.result_for_operation(
                    &operation,
                    "FAILED",
                    "Operation 回滚失败",
                    capability_for_action(&operation.action),
                    Vec::new(),
                )
                .map(|mut result| {
                    result.error = err.to_string();
                    result.message = format!("{}: {}", result.message, result.error);
                    result
                })
            }
        }
    }

    fn dispatch_operation_logs(&mut self, request: &ActionRequest) -> Result<ActionDispatchResult> {
        let operation_id = request.require_field("operation_id")?;
        let logs = self.store.list_operation_logs(operation_id)?;
        Ok(ActionDispatchResult {
            action_id: request.action.clone(),
            status: "READONLY".to_string(),
            message: format!("Operation {operation_id} 有 {} 条日志", logs.len()),
            operation_id: operation_id.to_string(),
            result: serde_json::json!({ "log_count": logs.len() }),
            error: String::new(),
            warnings: Vec::new(),
            changed_objects: Vec::new(),
            capability_status: ActionCapabilityStatus::Readonly,
            logs,
        })
    }

    fn with_operation_id(&self, mut request: ActionRequest) -> Result<ActionRequest> {
        if request.operation_id.trim().is_empty() {
            request.operation_id = next_operation_id(self.store, &request.action)?;
        }
        Ok(request)
    }

    fn result_for_operation(
        &self,
        operation: &Operation,
        status: impl Into<String>,
        message: impl Into<String>,
        capability_status: ActionCapabilityStatus,
        changed_objects: Vec<String>,
    ) -> Result<ActionDispatchResult> {
        let logs = self.store.list_operation_logs(&operation.operation_id)?;
        Ok(ActionDispatchResult {
            action_id: operation.action.clone(),
            status: status.into(),
            message: message.into(),
            operation_id: operation.operation_id.clone(),
            result: operation.result.clone(),
            error: operation.error_message.clone(),
            warnings: Vec::new(),
            changed_objects,
            capability_status,
            logs,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OrchestratorActionConsole {
    schemas: SharedSchemas,
    memory_store: MemoryOrchestratorStore,
    store_mode: ConsoleStoreMode,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConsoleStoreMode {
    Memory,
    Persistent { database_url: String },
}

impl OrchestratorActionConsole {
    pub fn load(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let repo_root = repo_root.into();
        let context = load_operation_workbench_context(&repo_root)?;
        Self::from_context(context)
    }

    #[cfg(test)]
    pub(crate) fn load_with_database_url(
        repo_root: impl Into<PathBuf>,
        database_url: Option<String>,
    ) -> Result<Self> {
        let repo_root = repo_root.into();
        let context = crate::workbench::load_operation_workbench_context_with_database_url(
            &repo_root,
            database_url,
        )?;
        Self::from_context(context)
    }

    fn from_context(context: OperationWorkbenchContext) -> Result<Self> {
        let schemas = context.schemas.clone();
        let store_mode = context
            .persistent_database_url()
            .map(|database_url| ConsoleStoreMode::Persistent {
                database_url: database_url.to_string(),
            })
            .unwrap_or(ConsoleStoreMode::Memory);
        let memory_store = memory_store_from_context(&context)?;
        Ok(Self {
            schemas,
            memory_store,
            store_mode,
            warnings: context.warnings.clone(),
        })
    }

    pub fn uses_persistent_store(&self) -> bool {
        matches!(self.store_mode, ConsoleStoreMode::Persistent { .. })
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn persistent_store(&self) -> Result<PgOrchestratorStore> {
        match &self.store_mode {
            ConsoleStoreMode::Memory => Err(OrchestratorError::Dependency(
                "persistent store URL is not configured".to_string(),
            )),
            ConsoleStoreMode::Persistent { database_url } => {
                PgOrchestratorStore::new(database_url.clone()).map_err(persistent_store_unavailable)
            }
        }
    }

    pub fn view(&self) -> Result<OrchestratorView> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            let mut view = load_orchestrator_view_from_store(self.schemas.clone(), &store)
                .map_err(persistent_store_unavailable)?;
            view.warnings.extend(self.warnings.clone());
            return Ok(view);
        }
        let mut view = load_orchestrator_view_from_store(self.schemas.clone(), &self.memory_store)?;
        view.warnings.extend(self.warnings.clone());
        Ok(view)
    }

    pub fn context(&self) -> Result<OperationWorkbenchContext> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            return load_operation_workbench_context_from_store(self.schemas.clone(), &store)
                .map_err(persistent_store_unavailable);
        }
        load_operation_workbench_context_from_store(self.schemas.clone(), &self.memory_store)
    }

    pub fn topology(&self) -> Result<Topology> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            return store
                .build_topology_view()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.build_topology_view()
    }

    pub fn operation(&self, operation_id: &str) -> Result<Option<Operation>> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            return store
                .get_operation(operation_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.get_operation(operation_id)
    }

    pub fn operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            return store
                .list_operation_logs(operation_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_operation_logs(operation_id)
    }

    pub fn diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            return store
                .get_diagnostic_report(report_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.get_diagnostic_report(report_id)
    }

    pub fn diagnostic_export(&self, report_id: &str, format: &str) -> Result<DiagnosticExport> {
        let report = self.diagnostic_report(report_id)?.ok_or_else(|| {
            OrchestratorError::Dependency(format!("diagnostic report {report_id} not found"))
        })?;
        export_diagnostic_report(&report, format)
    }

    pub fn release_registry(&self) -> Result<Vec<crate::ReleaseRegistryViewRow>> {
        Ok(self.view()?.release_registry)
    }

    pub fn service_releases(&self) -> Result<Vec<crate::ServiceRelease>> {
        if self.uses_persistent_store() {
            let store = self.persistent_store()?;
            return store
                .list_service_releases()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_service_releases()
    }

    pub fn dispatch(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        if self.uses_persistent_store() {
            let mut store = self.persistent_store()?;
            let result = OrchestratorActionDispatcher::new(&mut store).dispatch(request)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return Ok(result);
        }
        OrchestratorActionDispatcher::new(&mut self.memory_store).dispatch(request)
    }

    pub fn dispatch_with_static_probe(
        &mut self,
        request: ActionRequest,
    ) -> Result<ActionDispatchResult> {
        if self.uses_persistent_store() {
            let mut store = self.persistent_store()?;
            let result =
                OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
                    .dispatch(request)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return Ok(result);
        }
        OrchestratorActionDispatcher::with_endpoint_probe(
            &mut self.memory_store,
            StaticEndpointProbe,
        )
        .dispatch(request)
    }
}

pub fn action_matrix() -> Vec<ActionMatrixEntry> {
    action_catalog()
        .iter()
        .map(|descriptor| {
            let capability_status = capability_for_action(descriptor.action);
            ActionMatrixEntry {
                action_id: descriptor.action.to_string(),
                gui_entry: true,
                tui_entry: true,
                writes_store: writes_store(descriptor.action, capability_status),
                creates_operation: true,
                requires_confirmation: descriptor.plan_mode.requires_confirmation(),
                calls_executor: calls_executor(descriptor.action),
                capability_status,
            }
        })
        .collect()
}

pub fn capability_for_action(action: &str) -> ActionCapabilityStatus {
    match action {
        "endpoint.health.check" | "link.health.check" => ActionCapabilityStatus::Real,
        "endpoint.create"
        | "endpoint.update"
        | "endpoint.delete"
        | "link.create"
        | "link.update"
        | "link.delete"
        | "operation.create"
        | "operation.confirm"
        | "operation.apply"
        | "operation.cancel"
        | "operation.rollback"
        | "log.create"
        | "log.query"
        | "diagnostic.create"
        | "diagnostic.export"
        | "release.create"
        | "release.update"
        | "release.install"
        | "release.delete"
        | "release.rollback"
        | "route.create"
        | "route.update"
        | "route.delete"
        | "frontend.create"
        | "frontend.update"
        | "frontend.delete"
        | "migration.create"
        | "migration.update"
        | "migration.delete"
        | "permission.create"
        | "permission.update"
        | "permission.delete"
        | "redis.create"
        | "redis.update"
        | "redis.delete"
        | "storage.create"
        | "storage.update"
        | "storage.delete"
        | "config.create"
        | "config.update"
        | "config.delete"
        | "service.health.check" => ActionCapabilityStatus::StoreBacked,
        "release.list"
        | "release.get"
        | "release.validate"
        | "host.list"
        | "host.get"
        | "service.list"
        | "service.get"
        | "endpoint.list"
        | "endpoint.get"
        | "link.list"
        | "link.get"
        | "route.list"
        | "route.get"
        | "route.validate"
        | "frontend.list"
        | "frontend.get"
        | "frontend.validate"
        | "migration.list"
        | "migration.get"
        | "migration.validate"
        | "permission.list"
        | "permission.get"
        | "permission.validate"
        | "redis.list"
        | "redis.get"
        | "redis.validate"
        | "storage.list"
        | "storage.get"
        | "storage.validate"
        | "config.list"
        | "config.get"
        | "config.validate"
        | "secret.list"
        | "secret.get"
        | "topology.list"
        | "topology.get"
        | "topology.validate"
        | "topology.export"
        | "operation.list"
        | "operation.get"
        | "log.list"
        | "log.get"
        | "diagnostic.list"
        | "diagnostic.get" => ActionCapabilityStatus::Readonly,
        "service.start" | "service.stop" | "service.restart" | "service.enable"
        | "service.disable" | "service.delete" | "host.create" | "host.update" | "host.delete"
        | "host.health.check" | "service.create" | "service.update" | "route.apply"
        | "frontend.publish" | "migration.apply" | "migration.rollback" | "permission.sync"
        | "redis.apply" | "storage.apply" | "config.render" | "secret.create" | "secret.update"
        | "secret.delete" | "secret.distribute" | "topology.create" | "topology.update"
        | "topology.delete" | "topology.apply" | "operation.update" | "operation.delete"
        | "log.update" | "log.delete" | "diagnostic.update" | "diagnostic.delete" => {
            ActionCapabilityStatus::Unsupported
        }
        _ => ActionCapabilityStatus::Unsupported,
    }
}

fn readonly_result_for_action<S: OrchestratorStore>(
    store: &S,
    request: &ActionRequest,
) -> Result<Value> {
    match request.action.as_str() {
        "release.list" => Ok(serde_json::json!({
            "releases": filtered_service_releases(store.list_service_releases()?, request.field("service_id")),
        })),
        "release.get" => {
            let service_id = request.require_field("service_id")?;
            let version = request.field("version");
            let releases =
                filtered_service_releases(store.list_service_releases()?, Some(service_id));
            Ok(serde_json::json!({
                "release": releases.into_iter().find(|release| {
                    version.is_none_or(|version| release.version == version)
                }),
            }))
        }
        "release.validate" => {
            let service_id = request.field("service_id").unwrap_or("gateway");
            let service = store.get_service(service_id)?;
            let release =
                filtered_service_releases(store.list_service_releases()?, Some(service_id))
                    .into_iter()
                    .next();
            Ok(serde_json::json!({
                "status": if service.is_some() && release.is_some() { "ok" } else { "missing" },
                "service_id": service_id,
                "has_service": service.is_some(),
                "has_release": release.is_some(),
            }))
        }
        "service.list" => Ok(serde_json::json!({
            "services": store.list_services()?,
        })),
        "service.get" => {
            let service_id = request.require_field("service_id")?;
            Ok(serde_json::json!({
                "service": store.get_service(service_id)?,
            }))
        }
        "endpoint.list" => Ok(serde_json::json!({
            "endpoints": store.list_endpoints()?,
        })),
        "endpoint.get" => {
            let endpoint = request.require_field("endpoint")?;
            Ok(serde_json::json!({
                "endpoint": store.get_endpoint(endpoint)?,
            }))
        }
        "link.list" => Ok(serde_json::json!({
            "links": store.list_links()?,
        })),
        "link.get" => {
            let source = request.require_field("source_endpoint")?;
            let target = request.require_field("target_endpoint")?;
            Ok(serde_json::json!({
                "link": store.get_link(source, target)?,
            }))
        }
        "route.list" | "route.validate" => Ok(serde_json::json!({
            "routes": filtered_service_routes(store.list_service_routes()?, request.field("service_id")),
        })),
        "route.get" => {
            let route_id = request.require_field("route_id")?;
            Ok(serde_json::json!({
                "route": store
                    .list_service_routes()?
                    .into_iter()
                    .find(|route| route.path == route_id || format!("{} {}", route.method, route.path) == route_id),
            }))
        }
        "frontend.list" | "frontend.validate" => Ok(serde_json::json!({
            "frontends": filtered_service_frontends(store.list_service_frontend_entries()?, request.field("service_id")),
        })),
        "frontend.get" => {
            let frontend_id = request.require_field("frontend_id")?;
            Ok(serde_json::json!({
                "frontend": store
                    .list_service_frontend_entries()?
                    .into_iter()
                    .find(|frontend| frontend.service_name == frontend_id || frontend.route_prefix == frontend_id),
            }))
        }
        "migration.list" | "migration.validate" => Ok(serde_json::json!({
            "migrations": filtered_service_migrations(store.list_service_migration_records()?, request.field("service_id")),
        })),
        "migration.get" => {
            let migration_id = request.require_field("migration_id")?;
            Ok(serde_json::json!({
                "migration": store
                    .list_service_migration_records()?
                    .into_iter()
                    .find(|migration| migration.migration_version == migration_id || format!("{}@{}", migration.service_name, migration.migration_version) == migration_id),
            }))
        }
        "permission.list" | "permission.validate" => Ok(serde_json::json!({
            "permissions": filtered_service_permissions(store.list_service_permission_records()?, request.field("service_id")),
        })),
        "permission.get" => {
            let permission_id = request.require_field("permission_id")?;
            Ok(serde_json::json!({
                "permission": store
                    .list_service_permission_records()?
                    .into_iter()
                    .find(|permission| permission.permission_key == permission_id),
            }))
        }
        "redis.list" | "redis.validate" => Ok(serde_json::json!({
            "redis": filtered_service_redis(store.list_service_redis_resources()?, request.field("service_id")),
        })),
        "redis.get" => {
            let resource_id = request.require_field("resource_id")?;
            Ok(serde_json::json!({
                "redis": store
                    .list_service_redis_resources()?
                    .into_iter()
                    .find(|redis| redis.name == resource_id || format!("{}:{}", redis.service_name, redis.name) == resource_id),
            }))
        }
        "storage.list" | "storage.validate" => Ok(serde_json::json!({
            "storage": filtered_service_storage(store.list_service_storage_resources()?, request.field("service_id")),
        })),
        "storage.get" => {
            let resource_id = request.require_field("resource_id")?;
            Ok(serde_json::json!({
                "storage": store
                    .list_service_storage_resources()?
                    .into_iter()
                    .find(|storage| storage.object_type == resource_id || format!("{}:{}", storage.service_name, storage.object_type) == resource_id),
            }))
        }
        "config.list" | "config.validate" => Ok(serde_json::json!({
            "configs": filtered_service_configs(store.list_rendered_service_configs()?, request.field("service_id")),
        })),
        "config.get" => {
            let config_id = request.require_field("config_id")?;
            Ok(serde_json::json!({
                "config": store
                    .list_rendered_service_configs()?
                    .into_iter()
                    .find(|config| config.service_name == config_id || format!("{}@{}", config.service_name, config.version) == config_id),
            }))
        }
        "operation.list" => Ok(serde_json::json!({
            "operations": store.list_operations()?,
        })),
        "operation.get" => {
            let operation_id = request.require_field("operation_id")?;
            Ok(serde_json::json!({
                "operation": store.get_operation(operation_id)?,
            }))
        }
        "log.list" => Ok(serde_json::json!({
            "logs": store.list_log_sources()?,
        })),
        "log.get" => {
            let source_id = request.require_field("source_id")?;
            Ok(serde_json::json!({
                "log": store
                    .list_log_sources()?
                    .into_iter()
                    .find(|log| log.source_id == source_id),
            }))
        }
        "diagnostic.list" => Ok(serde_json::json!({
            "diagnostics": store.list_diagnostic_reports()?,
        })),
        "diagnostic.get" => {
            let report_id = request.require_field("report_id")?;
            Ok(serde_json::json!({
                "diagnostic": store.get_diagnostic_report(report_id)?,
            }))
        }
        "topology.get" | "topology.validate" | "topology.export" | "topology.list" => {
            let topology = store.build_topology_view()?;
            Ok(serde_json::to_value(topology)?)
        }
        _ => Ok(serde_json::json!({ "status": "READONLY", "action": request.action })),
    }
}

fn memory_store_from_context(
    context: &OperationWorkbenchContext,
) -> Result<MemoryOrchestratorStore> {
    let mut store = MemoryOrchestratorStore::new();
    for service in &context.services {
        store.put_service(service.clone())?;
    }
    for release in &context.releases {
        store.upsert_service_release(service_release_record(release)?)?;
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
    Ok(store)
}

fn memory_store_from_store<S: OrchestratorStore>(store: &S) -> Result<MemoryOrchestratorStore> {
    let mut memory = MemoryOrchestratorStore::new();
    for service in store.list_services()? {
        memory.put_service(service)?;
    }
    for release in store.list_service_releases()? {
        memory.upsert_service_release(release)?;
    }
    for route in store.list_service_routes()? {
        memory.upsert_service_route(route)?;
    }
    for migration in store.list_service_migration_records()? {
        memory.upsert_service_migration_record(migration)?;
    }
    for permission in store.list_service_permission_records()? {
        memory.upsert_service_permission_record(permission)?;
    }
    for frontend in store.list_service_frontend_entries()? {
        memory.upsert_service_frontend_entry(frontend)?;
    }
    for redis in store.list_service_redis_resources()? {
        memory.upsert_service_redis_resource(redis)?;
    }
    for storage in store.list_service_storage_resources()? {
        memory.upsert_service_storage_resource(storage)?;
    }
    for config in store.list_rendered_service_configs()? {
        memory.upsert_rendered_service_config(config)?;
    }
    for endpoint in store.list_endpoints()? {
        memory.put_endpoint(endpoint)?;
    }
    for link in store.list_links()? {
        memory.put_link(link)?;
    }
    for operation in store.list_operations()? {
        memory.put_operation(operation.clone())?;
        for log in store.list_operation_logs(&operation.operation_id)? {
            memory.append_operation_log(log)?;
        }
    }
    for log_view in store.list_log_sources()? {
        memory.put_log_view(log_view)?;
    }
    for report in store.list_diagnostic_reports()? {
        memory.put_diagnostic_report(report)?;
    }
    if let Ok(topology) = store.build_topology_view() {
        memory.put_topology(topology)?;
    }
    Ok(memory)
}

fn persistent_store_unavailable(err: OrchestratorError) -> OrchestratorError {
    OrchestratorError::Dependency(format!(
        "ORCHESTRATOR_DATABASE_URL store unavailable: {err}"
    ))
}

fn next_operation_id<S: OrchestratorStore>(store: &S, action: &str) -> Result<String> {
    let slug = action.replace('.', "-");
    Ok(format!("op-{slug}-{}", store.list_operations()?.len() + 1))
}

fn operation_result_status(operation: &Operation) -> String {
    operation
        .result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| match operation.status {
            OperationStatus::Planned => "PLANNED",
            OperationStatus::AwaitingConfirmation => "AWAITING_CONFIRMATION",
            OperationStatus::Running => "RUNNING",
            OperationStatus::Succeeded => "SUCCEEDED",
            OperationStatus::Failed => "FAILED",
            OperationStatus::RolledBack => "ROLLED_BACK",
            OperationStatus::Cancelled => "CANCELLED",
            OperationStatus::Expired => "EXPIRED",
        })
        .to_string()
}

fn changed_objects_from_result(result: &Value) -> Vec<String> {
    result
        .get("changed_objects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let object_type = value
                .get("object_type")
                .or_else(|| value.get("type"))
                .and_then(Value::as_str)?;
            let object_id = value
                .get("object_id")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)?;
            Some(format!("{object_type}:{object_id}"))
        })
        .collect()
}

fn writes_store(action: &str, capability: ActionCapabilityStatus) -> bool {
    matches!(
        capability,
        ActionCapabilityStatus::StoreBacked | ActionCapabilityStatus::Real
    ) || matches!(action, "log.query")
}

fn calls_executor(action: &str) -> bool {
    matches!(
        action,
        "endpoint.create"
            | "endpoint.update"
            | "endpoint.delete"
            | "endpoint.health.check"
            | "link.create"
            | "link.update"
            | "link.delete"
            | "link.health.check"
            | "route.create"
            | "route.update"
            | "route.delete"
            | "frontend.create"
            | "frontend.update"
            | "frontend.delete"
            | "migration.create"
            | "migration.update"
            | "migration.delete"
            | "permission.create"
            | "permission.update"
            | "permission.delete"
            | "redis.create"
            | "redis.update"
            | "redis.delete"
            | "storage.create"
            | "storage.update"
            | "storage.delete"
            | "config.create"
            | "config.update"
            | "config.delete"
            | "operation.apply"
            | "operation.rollback"
            | "diagnostic.create"
            | "diagnostic.export"
            | "log.create"
            | "log.query"
    )
}

fn is_service_lifecycle_action(action: &str) -> bool {
    matches!(
        action,
        "service.start"
            | "service.stop"
            | "service.restart"
            | "service.enable"
            | "service.disable"
            | "service.delete"
    )
}

fn filtered_service_releases(
    releases: Vec<crate::ServiceRelease>,
    service_id: Option<&str>,
) -> Vec<crate::ServiceRelease> {
    releases
        .into_iter()
        .filter(|release| service_id.is_none_or(|service_id| release.service_name == service_id))
        .collect()
}

fn filtered_service_routes(
    routes: Vec<crate::ServiceRoute>,
    service_id: Option<&str>,
) -> Vec<crate::ServiceRoute> {
    routes
        .into_iter()
        .filter(|route| service_id.is_none_or(|service_id| route.target_service_name == service_id))
        .collect()
}

fn filtered_service_frontends(
    frontends: Vec<crate::ServiceFrontendEntry>,
    service_id: Option<&str>,
) -> Vec<crate::ServiceFrontendEntry> {
    frontends
        .into_iter()
        .filter(|frontend| service_id.is_none_or(|service_id| frontend.service_name == service_id))
        .collect()
}

fn filtered_service_migrations(
    migrations: Vec<crate::ServiceMigrationRecord>,
    service_id: Option<&str>,
) -> Vec<crate::ServiceMigrationRecord> {
    migrations
        .into_iter()
        .filter(|migration| {
            service_id.is_none_or(|service_id| migration.service_name == service_id)
        })
        .collect()
}

fn filtered_service_permissions(
    permissions: Vec<crate::ServicePermissionRecord>,
    service_id: Option<&str>,
) -> Vec<crate::ServicePermissionRecord> {
    permissions
        .into_iter()
        .filter(|permission| {
            service_id.is_none_or(|service_id| permission.service_name == service_id)
        })
        .collect()
}

fn filtered_service_redis(
    resources: Vec<crate::ServiceRedisResource>,
    service_id: Option<&str>,
) -> Vec<crate::ServiceRedisResource> {
    resources
        .into_iter()
        .filter(|resource| service_id.is_none_or(|service_id| resource.service_name == service_id))
        .collect()
}

fn filtered_service_storage(
    resources: Vec<crate::ServiceStorageResource>,
    service_id: Option<&str>,
) -> Vec<crate::ServiceStorageResource> {
    resources
        .into_iter()
        .filter(|resource| service_id.is_none_or(|service_id| resource.service_name == service_id))
        .collect()
}

fn filtered_service_configs(
    configs: Vec<crate::RenderedServiceConfig>,
    service_id: Option<&str>,
) -> Vec<crate::RenderedServiceConfig> {
    configs
        .into_iter()
        .filter(|config| service_id.is_none_or(|service_id| config.service_name == service_id))
        .collect()
}

fn service_release_record(
    release: &crate::ServiceReleaseManifest,
) -> Result<crate::ServiceRelease> {
    Ok(crate::ServiceRelease {
        service_name: release.service_name.clone(),
        version: release.version.clone(),
        release_url: release.source.url.clone(),
        manifest: serde_json::to_value(release)?,
        checksum: release.source.checksum.clone(),
        created_at: String::new(),
    })
}

fn unsupported_reason(action: &str) -> &'static str {
    if is_service_lifecycle_action(action) {
        "driver action is unsupported: 该 Service 生命周期动作尚未接入真实执行器"
    } else {
        "driver action is unsupported: 该 action 当前暂不支持真实执行，已阻止假成功路径"
    }
}

fn action_target_id(request: &ActionRequest) -> String {
    request
        .field("target_id")
        .or_else(|| request.field("service_id"))
        .or_else(|| request.field("endpoint"))
        .or_else(|| request.field("operation_id"))
        .or_else(|| request.field("report_id"))
        .or_else(|| request.field("topology_snapshot_id"))
        .or_else(|| request.field("root_endpoint"))
        .unwrap_or("current")
        .to_string()
}

pub fn default_console_request(action: &str) -> Result<ActionRequest> {
    default_action_request(action)
        .ok_or_else(|| OrchestratorError::Dependency(format!("action {action} has no form")))
}
