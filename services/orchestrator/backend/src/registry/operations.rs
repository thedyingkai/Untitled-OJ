//! Closed repository-only mutations. No container drivers or deferred providers.
use crate::adapters::registry_compat::build_diagnostic_report;
use orchestrator_core::*;
use orchestrator_storage::OrchestratorStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn delete_release<S: OrchestratorStore>(
    store: &mut S,
    operation_id: String,
    service_id: &str,
    version: &str,
) -> Result<ActionDispatchResult> {
    MetadataOperation { store }.run(ActionRequest::new(
        operation_id,
        "release.delete",
        BTreeMap::from([
            ("service_id".into(), service_id.into()),
            ("version".into(), version.into()),
            ("confirm".into(), "true".into()),
        ]),
    ))
}

pub(super) fn create_diagnostic<S: OrchestratorStore>(
    store: &mut S,
) -> Result<ActionDispatchResult> {
    MetadataOperation { store }.run(ActionRequest::new("", "diagnostic.create", BTreeMap::new()))
}

struct MetadataOperation<'a, S: OrchestratorStore> {
    store: &'a mut S,
}
impl<S: OrchestratorStore> MetadataOperation<'_, S> {
    fn run(&mut self, request: ActionRequest) -> Result<ActionDispatchResult> {
        let request = self.with_operation_id(request)?;
        let services = self.store.list_services()?;
        let releases = self
            .store
            .list_service_releases()?
            .into_iter()
            .map(|record| crate::adapters::registry_compat::release_manifest(record.manifest))
            .collect::<Result<Vec<_>>>()?;
        let endpoints = self.store.list_endpoints()?;
        let topology = self
            .store
            .get_latest_topology_snapshot()?
            .map(|item| item.topology);
        let operation = plan_action_request_with_releases(
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
                ActionCapabilityStatus::StoreBacked,
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

        match self.apply(&operation.operation_id) {
            Ok(applied) => self.result_for_operation(
                &applied,
                operation_result_status(&applied),
                "Action 已通过 core dispatcher 执行",
                ActionCapabilityStatus::StoreBacked,
                changed_objects_from_result(&applied.result),
            ),
            Err(err) => {
                let operation = self
                    .store
                    .get_operation(&operation.operation_id)?
                    .unwrap_or(operation);
                // Preserve a failed metadata operation and its original error.
                self.result_for_operation(
                    &operation,
                    "FAILED",
                    "Action 执行失败",
                    ActionCapabilityStatus::StoreBacked,
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

    pub fn apply(&mut self, operation_id: &str) -> Result<Operation> {
        let operation = self
            .store
            .get_operation(operation_id)?
            .ok_or_else(|| OrchestratorError::Dependency("operation not found".to_string()))?;
        let requires_confirmation = operation
            .plan
            .get("requires_confirmation")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let can_apply = if requires_confirmation {
            matches!(operation.status, OperationStatus::AwaitingConfirmation)
        } else {
            matches!(
                operation.status,
                OperationStatus::Planned | OperationStatus::AwaitingConfirmation
            )
        };
        if !can_apply {
            return Err(OrchestratorError::Blocked(format!(
                "operation status {:?} cannot apply under current confirmation rule",
                operation.status
            )));
        }
        if operation
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
        {
            return Err(OrchestratorError::Blocked(
                "operation plan must contain at least one step".to_string(),
            ));
        }

        let lock_key = format!("operation:{operation_id}");
        let acquired = self.store.acquire_operation_lock(OperationLock {
            lock_key: lock_key.clone(),
            operation_id: operation_id.to_string(),
            owner: "orchestrator-core".to_string(),
            expires_at: "session".to_string(),
            created_at: String::new(),
        })?;
        if !acquired {
            return Err(OrchestratorError::Blocked(format!(
                "operation {operation_id} is locked"
            )));
        }

        let result = self.apply_with_acquired_lock(&operation);
        self.store.release_operation_lock(&lock_key, operation_id)?;
        result
    }

    fn apply_with_acquired_lock(&mut self, operation: &Operation) -> Result<Operation> {
        let running = start_operation(operation)?;
        self.store.update_operation(running.clone())?;
        self.store.append_operation_log(operation_log_record(
            &running.operation_id,
            "info",
            format!("operation {} started", running.action),
        ))?;
        for (index, step) in operation_steps(&running).iter().enumerate() {
            self.store.append_operation_log(operation_step_log_record(
                &running.operation_id,
                step_id(step, index),
                "info",
                format!("step {} planned", step_label(step)),
                step.clone(),
            ))?;
        }

        match self.apply_operation_mutation(&running) {
            Ok(changed_objects) => {
                let operation_after_mutation = self
                    .store
                    .get_operation(&running.operation_id)?
                    .unwrap_or_else(|| running.clone());
                let result = serde_json::json!({
                    "operation_id": running.operation_id,
                    "status": "SUCCEEDED",
                    "started_at": running.started_at,
                    "finished_at": "finished",
                    "changed_objects": changed_objects,
                    "topology_snapshot_id": serde_json::Value::Null,
                });
                let succeeded = succeed_operation(&operation_after_mutation, result)?;
                self.store.update_operation(succeeded.clone())?;
                self.store.append_operation_log(operation_log_record(
                    &succeeded.operation_id,
                    "info",
                    format!("operation {} succeeded", succeeded.action),
                ))?;
                Ok(succeeded)
            }
            Err(err) => {
                let operation_after_mutation = self
                    .store
                    .get_operation(&running.operation_id)?
                    .unwrap_or_else(|| running.clone());
                let failed = fail_operation(&operation_after_mutation, err.to_string())?;
                self.store.update_operation(failed.clone())?;
                self.store.append_operation_log(operation_log_record(
                    &failed.operation_id,
                    "error",
                    format!(
                        "operation {} failed: {}",
                        failed.action, failed.error_message
                    ),
                ))?;
                Err(err)
            }
        }
    }

    fn apply_operation_mutation(&mut self, operation: &Operation) -> Result<Vec<Value>> {
        let mut changed = Vec::new();
        match operation.action.as_str() {
            "release.delete" => {
                let service_name = operation
                    .request
                    .get("service_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let version = operation
                    .request
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                let referenced_by_deployment =
                    self.store.list_host_services()?.into_iter().any(|row| {
                        row.service_name == service_name
                            && version.is_none_or(|version| row.version == version)
                    }) || self
                        .store
                        .list_deployed_service_apis()?
                        .into_iter()
                        .any(|row| {
                            row.service_name == service_name
                                && version.is_none_or(|version| row.version == version)
                        });
                if referenced_by_deployment {
                    let target = version
                        .map(|version| format!("{service_name}@{version}"))
                        .unwrap_or_else(|| service_name.to_string());
                    return Err(OrchestratorError::Blocked(format!(
                        "release {target} is referenced by a deployment; install another version or use service.delete to uninstall it"
                    )));
                }
                let releases = self
                    .store
                    .list_service_releases()?
                    .into_iter()
                    .filter(|release| release.service_name == service_name)
                    .filter(|release| version.is_none_or(|version| release.version == version))
                    .collect::<Vec<_>>();
                if releases.is_empty() {
                    let target = version
                        .map(|version| format!("{service_name}@{version}"))
                        .unwrap_or_else(|| service_name.to_string());
                    return Err(OrchestratorError::Dependency(format!(
                        "release {target} not found"
                    )));
                }
                self.capture_release_delete_previous_state(operation, &releases)?;
                for release in releases {
                    self.store
                        .delete_service_release(&release.service_name, &release.version)?;
                    changed.push(changed_object(
                        "ServiceRelease",
                        &format!("{}@{}", release.service_name, release.version),
                    ));
                }
            }
            "diagnostic.create" => {
                let mut report = build_diagnostic_report(
                    self.store,
                    format!("diag-{}", operation.operation_id),
                )?;
                report.operation_id = operation.operation_id.clone();
                self.store.put_diagnostic_report(report.clone())?;
                changed.push(changed_object("DiagnosticReport", &report.report_id));
            }
            _ => {
                return Err(OrchestratorError::Blocked(
                    "only registry metadata operations are accepted".to_string(),
                ));
            }
        }
        Ok(changed)
    }

    fn capture_release_delete_previous_state(
        &mut self,
        operation: &Operation,
        releases: &[ServiceRelease],
    ) -> Result<ReleaseDeletePreviousState> {
        let previous_state = ReleaseDeletePreviousState {
            releases: releases.to_vec(),
        };
        let mut operation = operation.clone();
        operation
            .request
            .as_object_mut()
            .ok_or_else(|| {
                OrchestratorError::Dependency(
                    "release.delete operation request must be a JSON object".to_string(),
                )
            })?
            .insert(
                "previous_state".to_string(),
                serde_json::to_value(&previous_state)?,
            );
        self.store.update_operation(operation)?;
        Ok(previous_state)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct ReleaseDeletePreviousState {
    releases: Vec<ServiceRelease>,
}

fn operation_steps(operation: &Operation) -> Vec<serde_json::Value> {
    value_steps(&operation.plan)
}

fn value_steps(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn step_id(step: &serde_json::Value, index: usize) -> String {
    step.get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| step.get("action").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("step-{}", index + 1))
}

fn step_label(step: &serde_json::Value) -> String {
    step.get("action")
        .and_then(serde_json::Value::as_str)
        .or_else(|| step.as_str())
        .unwrap_or("operation-step")
        .to_string()
}

fn changed_object(object_type: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": object_type,
        "id": id
    })
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
