use crate::{
    ActionRequest, DiagnosticExport, DiagnosticReport, EndpointProbe, MemoryOrchestratorStore,
    Operation, OperationExecutor, OperationLogRecord, OperationStatus, OrchestratorError,
    OrchestratorStore, PgOrchestratorStore, Result, StaticEndpointProbe, TcpEndpointProbe,
    Topology, action_catalog, action_descriptor, confirm_operation, default_action_request,
    expand_set, export_diagnostic_report, load_operation_workbench_context,
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
            "operation.rollback" => self.dispatch_operation_rollback(&request),
            "operation.logs.view" => self.dispatch_operation_logs(&request),
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
        let sets = self.store.list_sets()?;
        let endpoints = self.store.list_endpoints()?;
        let topology = self
            .store
            .get_latest_topology_snapshot()?
            .map(|item| item.topology);
        let operation =
            plan_action_request(&request, &services, &sets, &endpoints, topology.as_ref())?;
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
        let sets = self.store.list_sets()?;
        let endpoints = self.store.list_endpoints()?;
        let topology = self
            .store
            .get_latest_topology_snapshot()?
            .map(|item| item.topology);
        let operation =
            plan_action_request(&request, &services, &sets, &endpoints, topology.as_ref())?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsoleStoreMode {
    Memory,
    PersistentFromEnv,
}

impl OrchestratorActionConsole {
    pub fn load(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let repo_root = repo_root.into();
        let context = load_operation_workbench_context(&repo_root)?;
        let schemas = context.schemas.clone();
        let store_mode = if context.uses_persistent_store() {
            ConsoleStoreMode::PersistentFromEnv
        } else {
            ConsoleStoreMode::Memory
        };
        let memory_store = memory_store_from_context(&context)?;
        Ok(Self {
            schemas,
            memory_store,
            store_mode,
            warnings: context.warnings.clone(),
        })
    }

    pub fn uses_persistent_store(&self) -> bool {
        matches!(self.store_mode, ConsoleStoreMode::PersistentFromEnv)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn view(&self) -> Result<OrchestratorView> {
        if self.uses_persistent_store() {
            match PgOrchestratorStore::from_env()
                .and_then(|store| load_orchestrator_view_from_store(self.schemas.clone(), &store))
            {
                Ok(mut view) => {
                    view.warnings.extend(self.warnings.clone());
                    return Ok(view);
                }
                Err(err) => {
                    let mut view = load_orchestrator_view_from_store(
                        self.schemas.clone(),
                        &self.memory_store,
                    )?;
                    view.warnings.push(format!(
                        "ORCHESTRATOR_DATABASE_URL store unavailable, using memory console view: {err}"
                    ));
                    view.warnings.extend(self.warnings.clone());
                    return Ok(view);
                }
            }
        }
        let mut view = load_orchestrator_view_from_store(self.schemas.clone(), &self.memory_store)?;
        view.warnings.extend(self.warnings.clone());
        Ok(view)
    }

    pub fn context(&self) -> Result<OperationWorkbenchContext> {
        if self.uses_persistent_store() {
            match PgOrchestratorStore::from_env().and_then(|store| {
                load_operation_workbench_context_from_store(self.schemas.clone(), &store)
            }) {
                Ok(context) => return Ok(context),
                Err(err) => {
                    let mut context = load_operation_workbench_context_from_store(
                        self.schemas.clone(),
                        &self.memory_store,
                    )?;
                    context.warnings.push(format!(
                        "ORCHESTRATOR_DATABASE_URL store unavailable, using memory console context: {err}"
                    ));
                    return Ok(context);
                }
            }
        }
        load_operation_workbench_context_from_store(self.schemas.clone(), &self.memory_store)
    }

    pub fn topology(&self) -> Result<Topology> {
        if self.uses_persistent_store() {
            match PgOrchestratorStore::from_env().and_then(|store| store.build_topology_view()) {
                Ok(topology) => return Ok(topology),
                Err(err) => {
                    let mut topology = self.memory_store.build_topology_view()?;
                    topology.authority.notes.push(format!(
                        "ORCHESTRATOR_DATABASE_URL store unavailable, using memory topology: {err}"
                    ));
                    return Ok(topology);
                }
            }
        }
        self.memory_store.build_topology_view()
    }

    pub fn operation(&self, operation_id: &str) -> Result<Option<Operation>> {
        if self.uses_persistent_store() {
            match PgOrchestratorStore::from_env()
                .and_then(|store| store.get_operation(operation_id))
            {
                Ok(operation) => return Ok(operation),
                Err(_) => return self.memory_store.get_operation(operation_id),
            }
        }
        self.memory_store.get_operation(operation_id)
    }

    pub fn operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        if self.uses_persistent_store() {
            match PgOrchestratorStore::from_env()
                .and_then(|store| store.list_operation_logs(operation_id))
            {
                Ok(logs) => return Ok(logs),
                Err(_) => return self.memory_store.list_operation_logs(operation_id),
            }
        }
        self.memory_store.list_operation_logs(operation_id)
    }

    pub fn diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        if self.uses_persistent_store() {
            match PgOrchestratorStore::from_env()
                .and_then(|store| store.get_diagnostic_report(report_id))
            {
                Ok(report) => return Ok(report),
                Err(_) => return self.memory_store.get_diagnostic_report(report_id),
            }
        }
        self.memory_store.get_diagnostic_report(report_id)
    }

    pub fn diagnostic_export(&self, report_id: &str, format: &str) -> Result<DiagnosticExport> {
        let report = self.diagnostic_report(report_id)?.ok_or_else(|| {
            OrchestratorError::Dependency(format!("diagnostic report {report_id} not found"))
        })?;
        export_diagnostic_report(&report, format)
    }

    pub fn dispatch(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        if self.uses_persistent_store() {
            let mut store = PgOrchestratorStore::from_env()?;
            let result = OrchestratorActionDispatcher::new(&mut store).dispatch(request)?;
            self.memory_store = memory_store_from_store(&store)?;
            return Ok(result);
        }
        OrchestratorActionDispatcher::new(&mut self.memory_store).dispatch(request)
    }

    pub fn dispatch_with_static_probe(
        &mut self,
        request: ActionRequest,
    ) -> Result<ActionDispatchResult> {
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
        "endpoint.register"
        | "endpoint.update"
        | "endpoint.delete"
        | "link.create"
        | "link.update"
        | "link.delete"
        | "set.apply"
        | "operation.plan"
        | "operation.confirm"
        | "operation.apply"
        | "operation.cancel"
        | "operation.rollback"
        | "diagnostics.run"
        | "diagnostics.export"
        | "service.install"
        | "service.logs.view"
        | "service.health.check" => ActionCapabilityStatus::StoreBacked,
        "service.start" | "service.stop" | "service.restart" | "service.enable"
        | "service.disable" | "service.delete" | "service.import" | "set.import"
        | "topology.apply" | "deployment.create" => ActionCapabilityStatus::Unsupported,
        _ => ActionCapabilityStatus::Readonly,
    }
}

fn readonly_result_for_action<S: OrchestratorStore>(
    store: &S,
    request: &ActionRequest,
) -> Result<Value> {
    match request.action.as_str() {
        "service.validate" => {
            let service_id = request.field("service_id").unwrap_or("gateway");
            let service = store.get_service(service_id)?;
            Ok(serde_json::json!({
                "status": if service.is_some() { "ok" } else { "missing" },
                "service_id": service_id,
            }))
        }
        "set.validate" | "set.expand" => {
            let set_id = request.require_field("set_id")?;
            let set = store
                .get_set(set_id)?
                .ok_or_else(|| OrchestratorError::Dependency(format!("set {set_id} not found")))?;
            let expanded = expand_set(&set);
            Ok(serde_json::json!({
                "status": "ok",
                "set_id": set.id,
                "services": expanded.services,
                "default_links": expanded.default_links,
            }))
        }
        "topology.load" | "topology.validate" | "topology.export" => {
            let topology = store.build_topology_view()?;
            Ok(serde_json::to_value(topology)?)
        }
        "deployment.open" | "deployment.diagnose" | "set.compare" => {
            Ok(serde_json::json!({ "status": "READONLY", "action": request.action }))
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
    Ok(store)
}

fn memory_store_from_store<S: OrchestratorStore>(store: &S) -> Result<MemoryOrchestratorStore> {
    let mut memory = MemoryOrchestratorStore::new();
    for service in store.list_services()? {
        memory.put_service(service)?;
    }
    for set in store.list_sets()? {
        memory.put_set(set)?;
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
    ) || matches!(action, "operation.logs.view")
}

fn calls_executor(action: &str) -> bool {
    matches!(
        action,
        "endpoint.register"
            | "endpoint.update"
            | "endpoint.delete"
            | "endpoint.health.check"
            | "link.create"
            | "link.update"
            | "link.delete"
            | "link.health.check"
            | "set.apply"
            | "operation.apply"
            | "operation.rollback"
            | "diagnostics.run"
            | "diagnostics.export"
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

fn unsupported_reason(action: &str) -> &'static str {
    if is_service_lifecycle_action(action) {
        "该 Service 生命周期动作尚未接入真实执行器"
    } else {
        "该 action 当前暂不支持真实执行，已阻止假成功路径"
    }
}

fn action_target_id(request: &ActionRequest) -> String {
    request
        .field("target_id")
        .or_else(|| request.field("service_id"))
        .or_else(|| request.field("set_id"))
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
