use crate::{
    ActionRequest, DeployedServiceApi, DiagnosticExport, DiagnosticReport, DriverResult, Endpoint,
    EndpointDecl, EndpointHealthResult, EndpointProbe, HostService, MemoryOrchestratorStore,
    NodeRecord, NodeServiceDispatchRequest, NodeServiceDispatchResult, Operation,
    OperationExecutor, OperationLogRecord, OperationStatus, OrchestratorError, OrchestratorStore,
    PgOrchestratorStore, RenderedServiceConfig, Result, RuntimeMode, ServiceApiSurface,
    ServiceHealthDecl, ServiceManifest, ServiceProvides, ServiceRequires, ServiceRuntimeDecl,
    SharedOrchestratorStore, SourceDecl, StaticEndpointProbe, TcpEndpointProbe, Topology,
    action_catalog, action_descriptor, cancel_operation, check_endpoint_health_with_probe,
    confirm_operation, default_action_request, export_diagnostic_report,
    load_operation_workbench_context, load_operation_workbench_context_from_store,
    load_orchestrator_view_from_store, operation_log_record, operation_step_log_record,
    plan_action_request, plan_operation, start_operation, succeed_operation, validate_endpoint,
    validate_host_service, validate_service_manifest, validate_service_release,
};
use crate::{
    ConfiguredAuthPermissionRegistrar, ConfiguredMigrationRunner,
    ConfiguredRedisResourceProvisioner, ConfiguredStorageResourceProvisioner,
};
use crate::{OperationWorkbenchContext, OrchestratorView, SharedSchemas};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionCapabilityStatus {
    Real,
    RuntimePipeline,
    StoreBacked,
    Unsupported,
    Readonly,
}

impl ActionCapabilityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Real => "REAL",
            Self::RuntimePipeline => "RUNTIME_PIPELINE",
            Self::StoreBacked => "STORE_BACKED",
            Self::Unsupported => "UNSUPPORTED",
            Self::Readonly => "READONLY",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionMatrixEntry {
    pub action_id: String,
    #[serde(alias = "gui_entry")]
    pub web_entry: bool,
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
            .map(|record| {
                crate::service_io::legacy_release_manifest_from_json_value(record.manifest)
            })
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
            format!("action {} planned by Web/TUI console", operation.action),
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

        let mut executor = OperationExecutor::with_runtime_provisioners(
            self.store,
            self.endpoint_probe.clone(),
            ConfiguredAuthPermissionRegistrar::from_env(),
            ConfiguredRedisResourceProvisioner::from_env(),
            ConfiguredStorageResourceProvisioner::from_env(),
            ConfiguredMigrationRunner::from_env(),
        );
        if request.field("execute_service_driver") == Some("true") {
            executor = executor.with_service_driver_execution_enabled();
        }
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
                // 执行失败就如实报 FAILED，并保留该 action 真实的 capability：
                // 把失败改写成 UNSUPPORTED 会让调用方以为「功能没做」而不是「这次执行炸了」，
                // 反而掩盖了错误。真正不支持的 action 在 dispatch() 入口就短路了，走不到这里。
                self.result_for_operation(
                    &operation,
                    "FAILED",
                    "Action 执行失败",
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
            "operation confirmed by Web/TUI console",
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
        // apply 是一次新的权限边界。调用方在这里授权驱动时，把事实写回
        // Operation，后续回滚才能知道原执行是否真的启动过运行时。
        if request.field("execute_service_driver") == Some("true") {
            let mut operation = self.store.get_operation(operation_id)?.ok_or_else(|| {
                OrchestratorError::Dependency(format!("operation {operation_id} not found"))
            })?;
            operation
                .request
                .as_object_mut()
                .ok_or_else(|| {
                    OrchestratorError::Dependency(format!(
                        "operation {operation_id} request must be a JSON object"
                    ))
                })?
                .insert("execute_service_driver".to_string(), Value::Bool(true));
            self.store.update_operation(operation)?;
        }
        let mut executor = OperationExecutor::with_runtime_provisioners(
            self.store,
            self.endpoint_probe.clone(),
            ConfiguredAuthPermissionRegistrar::from_env(),
            ConfiguredRedisResourceProvisioner::from_env(),
            ConfiguredStorageResourceProvisioner::from_env(),
            ConfiguredMigrationRunner::from_env(),
        );
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
            "operation cancelled by Web/TUI console",
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
        // 回滚与 apply 必须使用同一组运行时适配器。否则生命周期回滚会落到
        // DeferredGatewayRoutePublisher，且 local/container 驱动始终处于禁用状态，
        // 即使调用方明确传了 execute_service_driver=true 也只能得到 PLANNED。
        let mut executor = OperationExecutor::with_runtime_provisioners(
            self.store,
            self.endpoint_probe.clone(),
            ConfiguredAuthPermissionRegistrar::from_env(),
            ConfiguredRedisResourceProvisioner::from_env(),
            ConfiguredStorageResourceProvisioner::from_env(),
            ConfiguredMigrationRunner::from_env(),
        );
        if request.field("execute_service_driver") == Some("true") {
            executor = executor.with_service_driver_execution_enabled();
        }
        match executor.rollback(operation_id) {
            Ok(operation) => self.result_for_operation(
                &operation,
                "ROLLED_BACK",
                "Operation 已回滚",
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
    external_store: Option<SharedOrchestratorStore>,
    store_mode: ConsoleStoreMode,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConsoleStoreMode {
    Memory,
    Persistent { database_url: String },
    Injected { kind: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmokeControlPlaneSeed {
    pub root_node_id: String,
    pub root_host_ip: String,
    pub child_node_id: String,
    pub child_host_ip: String,
    pub storage_service_name: String,
    pub storage_version: String,
    pub storage_endpoint: String,
    pub storage_protocol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmokeNodeTreeSeed {
    pub root_node_id: String,
    pub root_host_ip: String,
    pub child_node_id: String,
    pub child_host_ip: String,
}

impl OrchestratorActionConsole {
    pub fn load(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let repo_root = repo_root.into();
        let context = load_operation_workbench_context(&repo_root)?;
        Self::from_context(context, Some(&repo_root))
    }

    pub fn load_with_database_url(
        repo_root: impl Into<PathBuf>,
        database_url: Option<String>,
    ) -> Result<Self> {
        let repo_root = repo_root.into();
        let context = crate::workbench::load_operation_workbench_context_with_database_url(
            &repo_root,
            database_url,
        )?;
        Self::from_context(context, Some(&repo_root))
    }

    fn from_context(context: OperationWorkbenchContext, repo_root: Option<&Path>) -> Result<Self> {
        let schemas = context.schemas.clone();
        let store_mode = context
            .persistent_database_url()
            .map(|database_url| ConsoleStoreMode::Persistent {
                database_url: database_url.to_string(),
            })
            .unwrap_or(ConsoleStoreMode::Memory);
        let memory_store = memory_store_from_context(&context)?;
        if let ConsoleStoreMode::Persistent { database_url } = &store_mode {
            let mut store = PgOrchestratorStore::new(database_url.clone())
                .map_err(persistent_store_unavailable)?;
            let repo_context = if let Some(repo_root) = repo_root {
                crate::workbench::load_operation_workbench_context_from_repo(repo_root)?
            } else {
                context.clone().with_memory_store()
            };
            sync_repo_manifest_registry_to_store(&mut store, &repo_context)
                .map_err(persistent_store_unavailable)?;
        }
        Ok(Self {
            schemas,
            memory_store,
            external_store: None,
            store_mode,
            warnings: context.warnings.clone(),
        })
    }

    /// Loads repository schemas while using an injected durable store as the
    /// sole mutable source of truth. This is the Desktop SQLite entrypoint and
    /// deliberately has no persistent-memory mirror.
    pub fn load_with_store(
        repo_root: impl Into<PathBuf>,
        kind: impl Into<String>,
        store: impl OrchestratorStore + Send + 'static,
    ) -> Result<Self> {
        let repo_root = repo_root.into();
        let context = crate::workbench::load_operation_workbench_context_from_repo(&repo_root)?;
        let schemas = context.schemas.clone();
        let memory_store = memory_store_from_context(&context)?;
        let mut external_store = SharedOrchestratorStore::new(kind.into(), store);
        sync_repo_manifest_registry_to_store(&mut external_store, &context)?;
        let kind = external_store.kind().to_string();
        Ok(Self {
            schemas,
            memory_store,
            external_store: Some(external_store),
            store_mode: ConsoleStoreMode::Injected { kind },
            warnings: context.warnings.clone(),
        })
    }

    pub fn uses_persistent_store(&self) -> bool {
        !matches!(self.store_mode, ConsoleStoreMode::Memory)
    }

    pub fn store_kind(&self) -> &str {
        match &self.store_mode {
            ConsoleStoreMode::Memory => "memory",
            ConsoleStoreMode::Persistent { .. } => "postgres",
            ConsoleStoreMode::Injected { kind } => kind,
        }
    }

    fn uses_postgres_store(&self) -> bool {
        matches!(self.store_mode, ConsoleStoreMode::Persistent { .. })
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn action_form(&self, action: &str) -> Option<&crate::ActionFormSchema> {
        self.schemas.form_for(action)
    }

    /// 市场导入：从外部 release 包源（GitHub Release 资产、任意 URL、本地路径）
    /// 拉取 release.yaml，合成并注册 Service + Release 契约。
    /// 注册完成后即可通过 `release.install` 动作安装该 Service。
    /// `package_root` 是包缓存与本地相对路径解析的根目录（通常为 repo root）。
    pub fn import_external_release(
        &mut self,
        package_root: &Path,
        source_url: &str,
        expected_checksum: Option<&str>,
    ) -> Result<crate::ExternalReleaseImport> {
        let loader = crate::LocalReleasePackageLoader::new(package_root);
        let fetched = loader.fetch_release_source(source_url)?;
        if let Some(expected) = expected_checksum
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && fetched.checksum != expected
        {
            return Err(OrchestratorError::InvalidManifest(format!(
                "release package checksum mismatch: expected {expected}, got {}",
                fetched.checksum
            )));
        }
        let mut import = crate::external_release_import_from_yaml(
            &fetched.release_yaml,
            source_url,
            &fetched.checksum,
        )?;
        if let Some(mut store) = self.external_store.clone() {
            crate::register_external_release_into_store(&mut store, &mut import)?;
        } else if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            crate::register_external_release_into_store(&mut store, &mut import)
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
        } else {
            crate::register_external_release_into_store(&mut self.memory_store, &mut import)?;
        }
        Ok(import)
    }

    /// Registers an already-fetched release document without performing any
    /// network I/O. The exact raw bytes are hashed again before parsing, and
    /// Service + Release publication uses the store's atomic write primitive.
    pub fn register_external_release_document(
        &mut self,
        document: &[u8],
        source_url: &str,
        expected_checksum: &str,
    ) -> Result<crate::ExternalReleaseImport> {
        let actual_checksum = format!("sha256:{:x}", Sha256::digest(document));
        if expected_checksum.trim() != actual_checksum {
            return Err(OrchestratorError::InvalidManifest(format!(
                "release metadata checksum mismatch: expected {}, got {actual_checksum}",
                expected_checksum.trim()
            )));
        }
        let text = std::str::from_utf8(document).map_err(|error| {
            OrchestratorError::InvalidManifest(format!(
                "release metadata must be UTF-8 YAML or JSON: {error}"
            ))
        })?;
        let mut import =
            crate::external_release_import_from_yaml(text, source_url, &actual_checksum)?;
        if let Some(mut store) = self.external_store.clone() {
            crate::register_external_release_into_store(&mut store, &mut import)?;
        } else if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            crate::register_external_release_into_store(&mut store, &mut import)
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
        } else {
            crate::register_external_release_into_store(&mut self.memory_store, &mut import)?;
        }
        Ok(import)
    }

    fn persistent_store(&self) -> Result<PgOrchestratorStore> {
        match &self.store_mode {
            ConsoleStoreMode::Memory => Err(OrchestratorError::Dependency(
                "persistent store URL is not configured".to_string(),
            )),
            ConsoleStoreMode::Persistent { database_url } => {
                PgOrchestratorStore::new(database_url.clone()).map_err(persistent_store_unavailable)
            }
            ConsoleStoreMode::Injected { .. } => Err(OrchestratorError::Dependency(
                "injected durable stores do not expose a PostgreSQL URL".to_string(),
            )),
        }
    }

    pub fn view(&self) -> Result<OrchestratorView> {
        if let Some(store) = &self.external_store {
            let mut view = load_orchestrator_view_from_store(self.schemas.clone(), store)?;
            view.warnings.extend(self.warnings.clone());
            return Ok(view);
        }
        if self.uses_postgres_store() {
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
        if let Some(store) = &self.external_store {
            return load_operation_workbench_context_from_store(self.schemas.clone(), store);
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return load_operation_workbench_context_from_store(self.schemas.clone(), &store)
                .map_err(persistent_store_unavailable);
        }
        load_operation_workbench_context_from_store(self.schemas.clone(), &self.memory_store)
    }

    pub fn topology(&self) -> Result<Topology> {
        if let Some(store) = &self.external_store {
            return store.build_topology_view();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .build_topology_view()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.build_topology_view()
    }

    pub fn operation(&self, operation_id: &str) -> Result<Option<Operation>> {
        if let Some(store) = &self.external_store {
            return store.get_operation(operation_id);
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .get_operation(operation_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.get_operation(operation_id)
    }

    pub fn operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        if let Some(store) = &self.external_store {
            return store.list_operation_logs(operation_id);
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_operation_logs(operation_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_operation_logs(operation_id)
    }

    pub fn diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        if let Some(store) = &self.external_store {
            return store.get_diagnostic_report(report_id);
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .get_diagnostic_report(report_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.get_diagnostic_report(report_id)
    }

    pub fn diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        if let Some(store) = &self.external_store {
            return store.list_diagnostic_reports();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_diagnostic_reports()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_diagnostic_reports()
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
        if let Some(store) = &self.external_store {
            return store.list_service_releases();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_service_releases()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_service_releases()
    }

    pub fn services(&self) -> Result<Vec<crate::ServiceManifest>> {
        if let Some(store) = &self.external_store {
            return store.list_services();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store.list_services().map_err(persistent_store_unavailable);
        }
        self.memory_store.list_services()
    }

    pub fn host_services(&self) -> Result<Vec<crate::HostService>> {
        if let Some(store) = &self.external_store {
            return store.list_host_services();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_host_services()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_host_services()
    }

    pub fn endpoints(&self) -> Result<Vec<crate::Endpoint>> {
        if let Some(store) = &self.external_store {
            return store.list_endpoints();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store.list_endpoints().map_err(persistent_store_unavailable);
        }
        self.memory_store.list_endpoints()
    }

    pub fn service_routes(&self) -> Result<Vec<crate::ServiceRoute>> {
        if let Some(store) = &self.external_store {
            return store.list_service_routes();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_service_routes()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_service_routes()
    }

    pub fn nodes(&self) -> Result<Vec<crate::NodeRecord>> {
        if let Some(store) = &self.external_store {
            return store.list_nodes();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store.list_nodes().map_err(persistent_store_unavailable);
        }
        self.memory_store.list_nodes()
    }

    pub fn node(&self, node_id: &str) -> Result<Option<crate::NodeRecord>> {
        if let Some(store) = &self.external_store {
            return store.get_node(node_id);
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .get_node(node_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.get_node(node_id)
    }

    pub fn upsert_node(&mut self, node: crate::NodeRecord) -> Result<crate::NodeRecord> {
        if let Some(mut store) = self.external_store.clone() {
            store.upsert_node(node.clone())?;
            return Ok(node);
        }
        if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            store
                .upsert_node(node.clone())
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return Ok(node);
        }
        self.memory_store.upsert_node(node.clone())?;
        Ok(node)
    }

    pub fn delete_node(&mut self, node_id: &str) -> Result<()> {
        if let Some(mut store) = self.external_store.clone() {
            return store.delete_node(node_id);
        }
        if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            store
                .delete_node(node_id)
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return Ok(());
        }
        self.memory_store.delete_node(node_id)
    }

    pub fn service_api_surfaces(&self) -> Result<Vec<crate::ServiceApiSurface>> {
        if let Some(store) = &self.external_store {
            return store.list_service_api_surfaces();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_service_api_surfaces()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_service_api_surfaces()
    }

    pub fn deployed_service_apis(&self) -> Result<Vec<crate::DeployedServiceApi>> {
        if let Some(store) = &self.external_store {
            return store.list_deployed_service_apis();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_deployed_service_apis()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_deployed_service_apis()
    }

    pub fn effective_api_routes(&self, node_id: &str) -> Result<Vec<crate::EffectiveApiRoute>> {
        if let Some(store) = &self.external_store {
            return store.effective_api_routes(node_id);
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .effective_api_routes(node_id)
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.effective_api_routes(node_id)
    }

    pub fn seed_smoke_control_plane(
        &mut self,
        seed: SmokeControlPlaneSeed,
    ) -> Result<Vec<crate::EffectiveApiRoute>> {
        if let Some(mut store) = self.external_store.clone() {
            seed_smoke_control_plane_into_store(&mut store, seed.clone())?;
            return store.effective_api_routes(&seed.child_node_id);
        }
        if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            seed_smoke_control_plane_into_store(&mut store, seed.clone())
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return store
                .effective_api_routes(&seed.child_node_id)
                .map_err(persistent_store_unavailable);
        }
        seed_smoke_control_plane_into_store(&mut self.memory_store, seed.clone())?;
        self.memory_store.effective_api_routes(&seed.child_node_id)
    }

    pub fn seed_smoke_node_tree(
        &mut self,
        seed: SmokeNodeTreeSeed,
    ) -> Result<Vec<crate::NodeRecord>> {
        if let Some(mut store) = self.external_store.clone() {
            seed_smoke_node_tree_into_store(&mut store, &seed)?;
            return store.list_nodes();
        }
        if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            seed_smoke_node_tree_into_store(&mut store, &seed)
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return store.list_nodes().map_err(persistent_store_unavailable);
        }
        seed_smoke_node_tree_into_store(&mut self.memory_store, &seed)?;
        self.memory_store.list_nodes()
    }

    pub fn service_permission_records(&self) -> Result<Vec<crate::ServicePermissionRecord>> {
        if let Some(store) = &self.external_store {
            return store.list_service_permission_records();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_service_permission_records()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_service_permission_records()
    }

    pub fn service_frontend_entries(&self) -> Result<Vec<crate::ServiceFrontendEntry>> {
        if let Some(store) = &self.external_store {
            return store.list_service_frontend_entries();
        }
        if self.uses_postgres_store() {
            let store = self.persistent_store()?;
            return store
                .list_service_frontend_entries()
                .map_err(persistent_store_unavailable);
        }
        self.memory_store.list_service_frontend_entries()
    }

    pub fn accept_node_service_install(
        &mut self,
        request: NodeServiceDispatchRequest,
    ) -> Result<NodeServiceDispatchResult> {
        if let Some(mut store) = self.external_store.clone() {
            return accept_node_service_install_into_store(&mut store, request);
        }
        if self.uses_postgres_store() {
            let mut store = self.persistent_store()?;
            let result = accept_node_service_install_into_store(&mut store, request)
                .map_err(persistent_store_unavailable)?;
            self.memory_store =
                memory_store_from_store(&store).map_err(persistent_store_unavailable)?;
            return Ok(result);
        }
        accept_node_service_install_into_store(&mut self.memory_store, request)
    }

    pub fn dispatch(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        if let Some(mut store) = self.external_store.clone() {
            return OrchestratorActionDispatcher::new(&mut store).dispatch(request);
        }
        if self.uses_postgres_store() {
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
        if let Some(mut store) = self.external_store.clone() {
            return OrchestratorActionDispatcher::with_endpoint_probe(
                &mut store,
                StaticEndpointProbe,
            )
            .dispatch(request);
        }
        if self.uses_postgres_store() {
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
                web_entry: true,
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
        // host.start / host.stop 走 executor：真的调驱动启停进程，再回写
        // HostService/DeployedServiceApi 状态并刷新 gateway 路由。
        // service.start / service.stop / service.restart 是同一条链路的单服务版本
        // （store.rs 的 apply_service_lifecycle），host.* 只是按 HostService 把它批量展开。
        // service.delete 同样先跑驱动动作，成功后才从 store 摘除 Service。
        "release.install" | "release.rollback" | "host.start" | "host.stop" | "service.start"
        | "service.stop" | "service.restart" | "service.delete" => {
            ActionCapabilityStatus::RuntimePipeline
        }
        "endpoint.create"
        | "endpoint.update"
        | "endpoint.delete"
        | "link.create"
        | "link.update"
        | "link.delete"
        | "link.enable"
        | "link.disable"
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
        | "release.delete"
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
        "service.enable" | "service.disable" | "host.create" | "host.update" | "host.delete"
        | "host.health.check" | "service.create" | "service.update" | "route.apply"
        | "frontend.publish" | "migration.apply" | "migration.rollback" | "permission.sync"
        | "redis.apply" | "storage.apply" | "config.render" | "secret.create" | "secret.update"
        | "secret.delete" | "secret.distribute" | "topology.create" | "topology.update"
        | "topology.delete" | "topology.apply" | "operation.create" | "operation.update"
        | "operation.delete" | "log.update" | "log.delete" | "diagnostic.update"
        | "diagnostic.delete" => ActionCapabilityStatus::Unsupported,
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
    sync_repo_manifest_registry_to_store(&mut store, context)?;
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

pub(crate) fn sync_repo_manifest_registry_to_store(
    store: &mut impl OrchestratorStore,
    context: &OperationWorkbenchContext,
) -> Result<()> {
    for service in &context.services {
        store.put_service(service.clone())?;
    }
    for release in &context.releases {
        let incoming = service_release_record(release)?;
        let existing_contract_version = store
            .get_service_release(&incoming.service_name, &incoming.version)?
            .and_then(|record| {
                record
                    .manifest
                    .get("schema_version")
                    .and_then(Value::as_u64)
            })
            .unwrap_or(1);
        let incoming_contract_version = incoming
            .manifest
            .get("schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        // Repository discovery currently exposes the normalized v1
        // projection. It may seed a missing row, but must never downgrade a
        // trusted Catalog v2 document already persisted for the same release.
        if existing_contract_version > incoming_contract_version {
            continue;
        }
        store.upsert_service_release(incoming)?;
    }
    Ok(())
}

fn memory_store_from_store<S: OrchestratorStore>(store: &S) -> Result<MemoryOrchestratorStore> {
    let mut memory = MemoryOrchestratorStore::new();
    for service in store.list_services()? {
        memory.put_service(service)?;
    }
    for release in store.list_service_releases()? {
        memory.upsert_service_release(release)?;
    }
    for host_service in store.list_host_services()? {
        memory.upsert_host_service(host_service)?;
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
    upsert_nodes_parent_first(&mut memory, store.list_nodes()?)?;
    for api in store.list_service_api_surfaces()? {
        memory.upsert_service_api_surface(api)?;
    }
    for endpoint in store.list_endpoints()? {
        memory.put_endpoint(endpoint)?;
    }
    for api in store.list_deployed_service_apis()? {
        memory.upsert_deployed_service_api(api)?;
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

pub(crate) fn upsert_nodes_parent_first(
    memory: &mut MemoryOrchestratorStore,
    nodes: Vec<crate::NodeRecord>,
) -> Result<()> {
    let mut pending = nodes;
    pending.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    while !pending.is_empty() {
        let mut next = Vec::new();
        let mut progressed = false;
        for node in pending {
            let parent = node.parent_node_id.trim();
            if parent.is_empty() || memory.get_node(parent)?.is_some() {
                memory.upsert_node(node)?;
                progressed = true;
            } else {
                next.push(node);
            }
        }
        if !progressed {
            let blocked = next
                .iter()
                .map(|node| format!("{} -> {}", node.node_id, node.parent_node_id))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(OrchestratorError::Dependency(format!(
                "node tree could not be loaded parent-first: {blocked}"
            )));
        }
        pending = next;
    }
    Ok(())
}

fn seed_smoke_control_plane_into_store<S: OrchestratorStore>(
    store: &mut S,
    seed: SmokeControlPlaneSeed,
) -> Result<()> {
    let storage_service_name = seed.storage_service_name.trim();
    let storage_version = seed.storage_version.trim();
    let endpoint_identity = crate::parse_endpoint_id(&seed.storage_endpoint)?;
    if endpoint_identity.service_name != storage_service_name {
        return Err(OrchestratorError::InvalidManifest(
            "smoke storage endpoint service-name must match storage_service_name".to_string(),
        ));
    }
    if endpoint_identity.host != seed.root_host_ip {
        return Err(OrchestratorError::InvalidManifest(
            "smoke storage endpoint host must match root_host_ip".to_string(),
        ));
    }
    let endpoint_port = endpoint_identity.port.parse::<u16>().map_err(|_| {
        OrchestratorError::InvalidManifest("smoke storage endpoint port is invalid".to_string())
    })?;

    seed_smoke_node_tree_into_store(
        store,
        &SmokeNodeTreeSeed {
            root_node_id: seed.root_node_id.clone(),
            root_host_ip: seed.root_host_ip.clone(),
            child_node_id: seed.child_node_id.clone(),
            child_host_ip: seed.child_host_ip.clone(),
        },
    )?;
    store.upsert_service(smoke_storage_service_manifest(
        storage_service_name,
        storage_version,
        endpoint_port,
    )?)?;
    store.upsert_host_service(HostService {
        host_ip: seed.root_host_ip.clone(),
        service_name: storage_service_name.to_string(),
        version: storage_version.to_string(),
        status: "running".to_string(),
        config: serde_json::json!({"smoke": true}),
        labels: serde_json::json!({"node_id": seed.root_node_id, "smoke": true}),
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    store.upsert_endpoint(Endpoint {
        endpoint: seed.storage_endpoint.clone(),
        service_id: storage_service_name.to_string(),
        protocol: seed.storage_protocol.clone(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: "Smoke storage-service".to_string(),
        note: "smoke/dev only endpoint seeded by OJOS_SMOKE_MODE".to_string(),
        config: serde_json::json!({"smoke": true}),
        created_at: String::new(),
        updated_at: String::new(),
    })?;

    for (api_id, methods, permission, auth_mode) in [
        (
            "storage.object.put",
            vec!["PUT".to_string(), "POST".to_string()],
            "storage.object.write",
            "service",
        ),
        (
            "storage.object.get",
            vec!["GET".to_string()],
            "storage.object.read",
            "service",
        ),
        (
            "storage.object.head",
            vec!["HEAD".to_string(), "GET".to_string()],
            "storage.object.read",
            "service",
        ),
        (
            "storage.object.delete",
            vec!["DELETE".to_string()],
            "storage.object.delete",
            "service",
        ),
        (
            "storage.object.public-head",
            vec!["HEAD".to_string(), "GET".to_string()],
            "public",
            "public",
        ),
    ] {
        store.upsert_service_api_surface(ServiceApiSurface {
            service_name: storage_service_name.to_string(),
            version: storage_version.to_string(),
            api_id: api_id.to_string(),
            protocol: seed.storage_protocol.clone(),
            port_name: "http".to_string(),
            path_prefix: "/api/storage/objects".to_string(),
            methods,
            visibility: "descendants".to_string(),
            auth_mode: auth_mode.to_string(),
            permission: permission.to_string(),
            stability: "stable".to_string(),
            api_version: "v1".to_string(),
            rate_limit: String::new(),
            timeout: "15s".to_string(),
            config: serde_json::json!({"smoke": true}),
            created_at: String::new(),
            updated_at: String::new(),
        })?;
        store.upsert_deployed_service_api(DeployedServiceApi {
            host_ip: seed.root_host_ip.clone(),
            service_name: storage_service_name.to_string(),
            version: storage_version.to_string(),
            endpoint: seed.storage_endpoint.clone(),
            api_id: api_id.to_string(),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })?;
    }
    Ok(())
}

fn seed_smoke_node_tree_into_store<S: OrchestratorStore>(
    store: &mut S,
    seed: &SmokeNodeTreeSeed,
) -> Result<()> {
    store.upsert_node(NodeRecord {
        node_id: seed.root_node_id.clone(),
        host_ip: seed.root_host_ip.clone(),
        parent_node_id: String::new(),
        role: "root".to_string(),
        labels: serde_json::json!({"smoke": true, "seed": "node-tree-only"}),
        status: "running".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    store.upsert_node(NodeRecord {
        node_id: seed.child_node_id.clone(),
        host_ip: seed.child_host_ip.clone(),
        parent_node_id: seed.root_node_id.clone(),
        role: "node".to_string(),
        labels: serde_json::json!({"smoke": true, "seed": "node-tree-only"}),
        status: "running".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    Ok(())
}

fn smoke_storage_service_manifest(
    service_id: &str,
    version: &str,
    default_port: u16,
) -> Result<ServiceManifest> {
    let manifest = ServiceManifest {
        schema_version: 1,
        id: service_id.to_string(),
        name: "Smoke Storage Service".to_string(),
        version: version.to_string(),
        kind: "backend-api".to_string(),
        description: "Smoke/dev only storage-service fixture".to_string(),
        endpoint: EndpointDecl {
            protocol: "http".to_string(),
            default_port,
            health_path: "/health".to_string(),
            expose: true,
            routes: vec!["/api/storage/objects".to_string()],
        },
        runtime: ServiceRuntimeDecl {
            mode: RuntimeMode::External,
            driver: "external".to_string(),
            root_allowed: true,
            non_root_allowed: false,
            start_policy: "manual".to_string(),
            restart_policy: "manual".to_string(),
        },
        config_schema: serde_json::json!({}),
        requires: ServiceRequires::default(),
        provides: ServiceProvides {
            capabilities: vec!["storage.object".to_string()],
            endpoints: Vec::new(),
            routes: vec!["/api/storage/objects".to_string()],
            workers: Vec::new(),
            storage_buckets: Vec::new(),
            events: Vec::new(),
        },
        ui: Default::default(),
        permissions: vec![
            "storage.object.read".to_string(),
            "storage.object.write".to_string(),
            "storage.object.delete".to_string(),
        ],
        security: Default::default(),
        source: SourceDecl {
            r#type: "local".to_string(),
            reference: "services/storage-service".to_string(),
            build: serde_json::json!({}),
            artifact: serde_json::json!({}),
        },
        health: ServiceHealthDecl {
            checks: vec!["http".to_string()],
            timeout_seconds: 3,
            interval_seconds: 10,
        },
        resources: serde_json::json!({"smoke": true}),
    };
    validate_service_manifest(&manifest)?;
    Ok(manifest)
}

fn accept_node_service_install_into_store<S: OrchestratorStore>(
    store: &mut S,
    request: NodeServiceDispatchRequest,
) -> Result<NodeServiceDispatchResult> {
    validate_service_manifest(&request.service)?;
    validate_host_service(&request.host_service)?;
    validate_endpoint(&request.endpoint)?;
    if let Some(release) = request.release.as_ref() {
        validate_service_release(release)?;
        if release.service_name != request.service.id {
            return Err(OrchestratorError::InvalidManifest(
                "node install release service_name must match service id".to_string(),
            ));
        }
        if release.version != request.service.version {
            return Err(OrchestratorError::InvalidManifest(
                "node install release version must match service version".to_string(),
            ));
        }
        if release.service_type != request.service.kind {
            return Err(OrchestratorError::InvalidManifest(
                "node install release service_type must match service kind".to_string(),
            ));
        }
        if release.backend.protocol != request.service.endpoint.protocol
            || release.backend.port != request.service.endpoint.default_port
            || release.backend.health_path != request.service.endpoint.health_path
        {
            return Err(OrchestratorError::InvalidManifest(
                "node install release backend must match service endpoint".to_string(),
            ));
        }
    }
    if request.host_service.service_name != request.service.id {
        return Err(OrchestratorError::InvalidManifest(
            "node install host_service service_name must match service id".to_string(),
        ));
    }
    if request.host_service.version != request.service.version {
        return Err(OrchestratorError::InvalidManifest(
            "node install host_service version must match service version".to_string(),
        ));
    }
    if request.endpoint.service_id != request.service.id {
        return Err(OrchestratorError::InvalidManifest(
            "node install endpoint service_id must match service id".to_string(),
        ));
    }
    let identity = crate::parse_endpoint_id(&request.endpoint.endpoint)?;
    if identity.host != request.host_service.host_ip {
        return Err(OrchestratorError::InvalidManifest(
            "node install endpoint host must match host_service host_ip".to_string(),
        ));
    }

    let mut operation = node_install_operation_from_request(&request)?;
    store.update_operation(operation.clone())?;
    operation = start_operation(&operation)?;
    store.update_operation(operation.clone())?;
    store.append_operation_log(operation_step_log_record(
        &operation.operation_id,
        "node-accept",
        "info",
        format!(
            "node orchestrator accepted install dispatch for {}@{}",
            request.service.id, request.service.version
        ),
        serde_json::json!({
            "service_id": request.service.id,
            "version": request.service.version,
            "endpoint": request.endpoint.endpoint,
            "host_ip": request.host_service.host_ip,
            "package_load": request.package_load,
            "execute_service_driver": request.execute_service_driver,
        }),
    ))?;

    let mut host_service = request.host_service.clone();
    let mut endpoint = request.endpoint.clone();
    let mut rendered_config = request.rendered_config.clone();
    let driver_result = match node_install_driver_result(&request) {
        Ok(result) => result,
        Err(err) => {
            let failed = crate::fail_operation(&operation, err.to_string())?;
            store.update_operation(failed.clone())?;
            store.append_operation_log(operation_step_log_record(
                &failed.operation_id,
                "node-driver",
                "error",
                format!("node service driver failed before execution: {err}"),
                serde_json::json!({
                    "service_id": request.service.id,
                    "version": request.service.version,
                    "endpoint": request.endpoint.endpoint,
                    "error": err.to_string(),
                }),
            ))?;
            return Err(err);
        }
    };
    let health = node_install_health_result(&endpoint, driver_result.as_ref())?;
    if let Some(driver_result) = driver_result.as_ref() {
        host_service.status =
            if driver_result.status.eq_ignore_ascii_case("SUCCEEDED") && !health.reachable {
                "failed".to_string()
            } else {
                crate::store::release_install_host_status(
                    Some(&NodeServiceDispatchResult {
                        status: "accepted".to_string(),
                        message: String::new(),
                        endpoint: endpoint.endpoint.clone(),
                        accepted: true,
                        driver_executed: true,
                        driver_status: driver_result.status.clone(),
                    }),
                    driver_result,
                    &health,
                )
                .to_string()
            };
        endpoint.health = health.health.clone();
        endpoint.reachable = health.reachable;
        set_rendered_external_step(
            &mut rendered_config,
            "node_driver",
            serde_json::json!({
                "status": driver_result.status,
                "message": driver_result.message,
                "command": driver_result.command,
            }),
        );
        set_rendered_external_step(
            &mut rendered_config,
            "node_health",
            serde_json::json!({
                "status": health.health,
                "reachable": health.reachable,
                "message": health.message,
            }),
        );
    }

    store.upsert_service(request.service.clone())?;
    store.upsert_host_service(host_service.clone())?;
    store.upsert_rendered_service_config(RenderedServiceConfig {
        service_name: request.service.id.clone(),
        version: request.service.version.clone(),
        config: rendered_config,
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    store.upsert_endpoint(endpoint.clone())?;
    store.append_operation_log(operation_step_log_record(
        &operation.operation_id,
        "node-store",
        "info",
        format!(
            "node registry stored service {}, host {}, endpoint {}",
            request.service.id, host_service.host_ip, endpoint.endpoint
        ),
        serde_json::json!({
            "service_id": request.service.id,
            "version": request.service.version,
            "host_ip": host_service.host_ip,
            "host_status": host_service.status,
            "endpoint": endpoint.endpoint,
            "endpoint_health": endpoint.health,
            "endpoint_reachable": endpoint.reachable,
        }),
    ))?;
    if let Some(driver_result) = driver_result.as_ref() {
        store.append_operation_log(node_driver_log_record(
            &operation.operation_id,
            driver_result,
        ))?;
    } else {
        store.append_operation_log(operation_step_log_record(
            &operation.operation_id,
            "node-driver",
            "warn",
            "node service driver execution deferred",
            serde_json::json!({
                "status": "deferred",
                "execute_env": "ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER",
                "request_authorized": request.execute_service_driver,
                "node_execution_enabled": dispatcher_env_flag(
                    "ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER"
                ),
            }),
        ))?;
    }
    store.append_operation_log(node_health_log_record(&operation.operation_id, &health))?;

    let driver_executed = driver_result.is_some();
    let driver_status = driver_result
        .as_ref()
        .map(|driver| driver.status.clone())
        .unwrap_or_else(|| "DEFERRED".to_string());
    let driver_failed = driver_result
        .as_ref()
        .is_some_and(|driver| !driver.status.eq_ignore_ascii_case("SUCCEEDED"));
    let health_failed = driver_result
        .as_ref()
        .is_some_and(|driver| driver.status.eq_ignore_ascii_case("SUCCEEDED") && !health.reachable);
    let execution_failed = driver_failed || health_failed;
    let result = NodeServiceDispatchResult {
        status: if execution_failed {
            "failed".to_string()
        } else {
            "accepted".to_string()
        },
        message: node_install_message(
            &request.service.id,
            &host_service.host_ip,
            driver_result.as_ref(),
            &health,
        ),
        endpoint: endpoint.endpoint,
        accepted: true,
        driver_executed,
        driver_status,
    };
    let operation_result = serde_json::json!({
        "operation_id": operation.operation_id,
        "status": if execution_failed {
            "FAILED"
        } else {
            "SUCCEEDED"
        },
        "service_id": request.service.id,
        "version": request.service.version,
        "host_ip": host_service.host_ip,
        "endpoint": result.endpoint,
        "node_dispatch_result": result,
        "driver": driver_result,
        "health": health,
    });
    let finished = if execution_failed {
        let error = if let Some(driver) = driver_result
            .as_ref()
            .filter(|driver| !driver.status.eq_ignore_ascii_case("SUCCEEDED"))
        {
            if driver.status.eq_ignore_ascii_case("FAILED") {
                format!("node service driver failed: {}", driver.message)
            } else {
                format!(
                    "node service driver did not succeed ({}): {}",
                    driver.status, driver.message
                )
            }
        } else {
            format!(
                "node service health failed for {}: {}; the runtime may still be running and must be stopped on this node before retrying",
                request.endpoint.endpoint, health.message
            )
        };
        crate::fail_operation(&operation, error)?
    } else {
        succeed_operation(&operation, operation_result.clone())?
    };
    let mut finished = finished;
    if matches!(finished.status, OperationStatus::Failed) {
        finished.result = operation_result;
    }
    store.update_operation(finished.clone())?;
    store.append_operation_log(operation_log_record(
        &finished.operation_id,
        if matches!(finished.status, OperationStatus::Failed) {
            "error"
        } else {
            "info"
        },
        format!(
            "node install operation {}",
            if matches!(finished.status, OperationStatus::Failed) {
                "failed"
            } else {
                "succeeded"
            }
        ),
    ))?;

    Ok(result)
}

fn node_install_operation_from_request(request: &NodeServiceDispatchRequest) -> Result<Operation> {
    plan_operation(
        request.operation_id.clone(),
        "release.install",
        "NodeServiceInstall",
        request.service.id.clone(),
        serde_json::json!({
            "service_id": request.service.id,
            "version": request.service.version,
            "endpoint": request.endpoint.endpoint,
            "host_ip": request.host_service.host_ip,
            "node_mode": true,
            "package_load": request.package_load,
            "execute_service_driver": request.execute_service_driver,
            "release_manifest": request.release,
        }),
        serde_json::json!({
            "steps": [
                {
                    "id": "node-accept",
                    "action": "node.install.accept",
                    "target": request.service.id,
                    "detail": "accept root orchestrator dispatch"
                },
                {
                    "id": "node-store",
                    "action": "node.install.store",
                    "target": request.endpoint.endpoint,
                    "detail": "write node-side service, host service, endpoint, rendered config"
                },
                {
                    "id": "node-driver",
                    "action": "node.install.driver",
                    "target": request.service.id,
                    "detail": "run local service driver when enabled"
                },
                {
                    "id": "node-health",
                    "action": "node.install.health",
                    "target": request.endpoint.endpoint,
                    "detail": "probe endpoint after successful driver execution"
                }
            ],
            "requires_confirmation": false
        }),
        serde_json::json!({
            "steps": [],
            "supported": false,
            "reason": "node-side service install rollback is not implemented"
        }),
    )
}

fn node_driver_log_record(operation_id: &str, result: &DriverResult) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        "node-driver",
        if result.status.eq_ignore_ascii_case("SUCCEEDED") {
            "info"
        } else {
            "error"
        },
        format!(
            "node service driver returned {}: {}",
            result.status, result.message
        ),
        serde_json::json!({
            "action": result.action,
            "status": result.status,
            "message": result.message,
            "command": result.command,
        }),
    )
}

fn node_health_log_record(operation_id: &str, result: &EndpointHealthResult) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        "node-health",
        if result.reachable { "info" } else { "warn" },
        format!(
            "node endpoint health {} reachable={}",
            result.health, result.reachable
        ),
        serde_json::json!({
            "endpoint": result.endpoint,
            "health": result.health,
            "reachable": result.reachable,
            "latency_ms": result.latency_ms,
            "message": result.message,
        }),
    )
}

fn set_rendered_external_step(config: &mut Value, key: &str, value: Value) {
    if !config.is_object() {
        *config = serde_json::json!({});
    }
    let object = config
        .as_object_mut()
        .expect("config was normalized to object");
    let external_steps = object
        .entry("external_steps".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !external_steps.is_object() {
        *external_steps = serde_json::json!({});
    }
    external_steps
        .as_object_mut()
        .expect("external_steps was normalized to object")
        .insert(key.to_string(), value);
}

fn node_install_driver_result(
    request: &NodeServiceDispatchRequest,
) -> Result<Option<DriverResult>> {
    if !request.execute_service_driver {
        return Ok(None);
    }
    if !dispatcher_env_flag("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER") {
        return Err(OrchestratorError::Blocked(
            "node driver authorization was requested, but ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER is not enabled"
                .to_string(),
        ));
    }
    ensure_node_install_target_identity(request)?;
    let operation = node_install_operation_from_request(request)?;
    crate::store::execute_service_driver_action(&request.service, &operation, true).map(Some)
}

fn ensure_node_install_target_identity(request: &NodeServiceDispatchRequest) -> Result<()> {
    let configured_host = std::env::var("ORCHESTRATOR_NODE_HOST_IP")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OrchestratorError::Blocked(
                "node driver execution requires ORCHESTRATOR_NODE_HOST_IP".to_string(),
            )
        })?
        .parse::<std::net::IpAddr>()
        .map_err(|_| {
            OrchestratorError::Blocked(
                "ORCHESTRATOR_NODE_HOST_IP must be a valid IP address".to_string(),
            )
        })?;
    let request_host = request
        .host_service
        .host_ip
        .trim()
        .parse::<std::net::IpAddr>()
        .map_err(|_| {
            OrchestratorError::Blocked(
                "node install host_service.host_ip must be a valid IP address".to_string(),
            )
        })?;
    let endpoint_host = crate::parse_endpoint_id(&request.endpoint.endpoint)?
        .host
        .parse::<std::net::IpAddr>()
        .map_err(|_| {
            OrchestratorError::Blocked(
                "node install endpoint host must be a valid IP address".to_string(),
            )
        })?;
    if configured_host != request_host || configured_host != endpoint_host {
        return Err(OrchestratorError::Blocked(format!(
            "node install target identity mismatch: configured host {configured_host}, request host {request_host}, endpoint host {endpoint_host}"
        )));
    }
    Ok(())
}

fn dispatcher_env_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn node_install_health_result(
    endpoint: &crate::Endpoint,
    driver_result: Option<&DriverResult>,
) -> Result<EndpointHealthResult> {
    let Some(driver_result) = driver_result else {
        return Ok(EndpointHealthResult {
            endpoint: endpoint.endpoint.clone(),
            health: endpoint.health.clone(),
            reachable: endpoint.reachable,
            latency_ms: None,
            message: "node health probe deferred until driver execution is enabled".to_string(),
        });
    };
    if driver_result.status.eq_ignore_ascii_case("SUCCEEDED") {
        let attempts = std::env::var("ORCHESTRATOR_ENDPOINT_HEALTH_ATTEMPTS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(25)
            .max(1);
        let interval = Duration::from_millis(
            std::env::var("ORCHESTRATOR_ENDPOINT_HEALTH_INTERVAL_MS")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(200),
        );
        let probe = TcpEndpointProbe::new(Duration::from_millis(800));
        let mut last = None;
        for attempt in 0..attempts {
            let health = check_endpoint_health_with_probe(endpoint, &probe)?;
            if health.reachable {
                return Ok(health);
            }
            last = Some(health);
            if attempt + 1 < attempts {
                std::thread::sleep(interval);
            }
        }
        last.ok_or_else(|| {
            OrchestratorError::Dependency(
                "node endpoint health wait produced no result".to_string(),
            )
        })
    } else {
        Ok(EndpointHealthResult {
            endpoint: endpoint.endpoint.clone(),
            health: "deferred".to_string(),
            reachable: false,
            latency_ms: None,
            message: format!(
                "node health probe deferred until driver succeeds; current driver status {}",
                driver_result.status
            ),
        })
    }
}

fn node_install_message(
    service_id: &str,
    host_ip: &str,
    driver_result: Option<&DriverResult>,
    health: &EndpointHealthResult,
) -> String {
    match driver_result {
        Some(driver) if driver.status.eq_ignore_ascii_case("SUCCEEDED") && !health.reachable => {
            format!(
                "node orchestrator started {service_id} on {host_ip}, but health is {}; the runtime may still be running and must be stopped on this node before retrying",
                health.health
            )
        }
        Some(driver) => format!(
            "node orchestrator accepted {service_id} on {host_ip}; driver {}; health {}",
            driver.status, health.health
        ),
        None => format!(
            "node orchestrator accepted {service_id} on {host_ip}; driver execution deferred"
        ),
    }
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
        .unwrap_or(match operation.status {
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
        ActionCapabilityStatus::StoreBacked
            | ActionCapabilityStatus::RuntimePipeline
            | ActionCapabilityStatus::Real
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
            | "link.enable"
            | "link.disable"
            | "link.health.check"
            | "release.install"
            | "release.delete"
            | "release.rollback"
            | "host.start"
            | "host.stop"
            | "service.start"
            | "service.stop"
            | "service.restart"
            | "service.delete"
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

/// 仍未接通真实执行链的 Service 生命周期动作，目前只剩 enable/disable：
/// 它们在 executor 里只跑一次驱动动作，不回写 HostService/DeployedServiceApi 状态，
/// 也不刷新 gateway 路由，因此 capability 保持 Unsupported，并在 dispatch_unsupported
/// 里给出比通用文案更准确的原因。
///
/// service.start / stop / restart / delete 已接入 RuntimePipeline：它们执行失败时
/// 必须如实报 FAILED + RUNTIME_PIPELINE，不能再降级成 UNSUPPORTED 掩盖真实错误。
fn is_unsupported_service_lifecycle_action(action: &str) -> bool {
    matches!(action, "service.enable" | "service.disable")
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
    if is_unsupported_service_lifecycle_action(action) {
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

#[cfg(test)]
mod trusted_release_document_tests {
    use super::{OrchestratorActionConsole, sync_repo_manifest_registry_to_store};
    use crate::OrchestratorStore;
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    #[test]
    fn checksum_mismatch_cannot_publish_service_or_release() {
        let root = workspace_root();
        let bytes = std::fs::read(root.join("services/gateway/release.yaml"))
            .expect("read fixture release");
        let mut console =
            OrchestratorActionConsole::load_with_database_url(&root, None).expect("console");
        let services_before = console.services().expect("services before");
        let releases_before = console.service_releases().expect("releases before");
        let error = console
            .register_external_release_document(
                &bytes,
                "services/gateway/release.yaml",
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            )
            .expect_err("checksum mismatch must fail");
        assert!(error.to_string().contains("checksum mismatch"));
        assert_eq!(console.services().expect("services after"), services_before);
        assert_eq!(
            console.service_releases().expect("releases after"),
            releases_before
        );
    }

    #[test]
    fn registered_v2_document_keeps_versioned_contract_sections() {
        let root = workspace_root();
        let bytes = std::fs::read(root.join("services/judge-worker/release.yaml"))
            .expect("read v2 release fixture");
        let checksum = format!("sha256:{:x}", Sha256::digest(&bytes));
        let mut console =
            OrchestratorActionConsole::load_with_database_url(&root, None).expect("console");

        console
            .register_external_release_document(
                &bytes,
                "https://catalog.example/judge-worker/release.yaml",
                &checksum,
            )
            .expect("register v2 document");

        let record = console
            .service_releases()
            .expect("registered releases")
            .into_iter()
            .find(|release| release.service_name == "judge-worker" && release.version == "0.1.0")
            .expect("judge-worker release record");
        assert_eq!(record.manifest["schema_version"], 2);
        assert!(record.manifest["provides"].is_object());
        assert_eq!(
            record.manifest["requires"]["apis"][0]["name"],
            "judge_control"
        );
        assert!(record.manifest["events"].is_object());
        assert_eq!(
            record.manifest["runtime_contract"]["id"],
            "judge-sandbox-v1"
        );
    }

    #[test]
    fn repository_bootstrap_cannot_downgrade_an_imported_v2_record() {
        let root = workspace_root();
        let bytes = std::fs::read(root.join("services/judge-worker/release.yaml"))
            .expect("read v2 release fixture");
        let checksum = format!("sha256:{:x}", Sha256::digest(&bytes));
        let text = std::str::from_utf8(&bytes).unwrap();
        let mut import = crate::external_release_import_from_yaml(
            text,
            "https://catalog.example/judge-worker/release.yaml",
            &checksum,
        )
        .expect("v2 import");
        let mut store = crate::MemoryOrchestratorStore::new();
        crate::register_external_release_into_store(&mut store, &mut import)
            .expect("register imported v2 release");
        let context = crate::workbench::load_operation_workbench_context_from_repo(&root)
            .expect("repository context");

        sync_repo_manifest_registry_to_store(&mut store, &context)
            .expect("repository bootstrap sync");

        let record = store
            .get_service_release("judge-worker", "0.1.0")
            .unwrap()
            .expect("judge-worker release record");
        assert_eq!(record.manifest["schema_version"], 2);
        assert_eq!(
            record.manifest["requires"]["apis"][0]["name"],
            "judge_control"
        );
    }
}
