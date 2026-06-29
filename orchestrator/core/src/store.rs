use crate::{
    DiagnosticReport, Endpoint, Link, LogView, Operation, OperationLogRecord, OperationStatus,
    OrchestratorError, Result, ServiceManifest, ServiceSet, Topology, operation_log_record,
    validate_endpoint, validate_link, validate_topology,
};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

pub trait OrchestratorStore {
    fn put_service(&mut self, service: ServiceManifest) -> Result<()>;
    fn put_set(&mut self, set: ServiceSet) -> Result<()>;
    fn put_endpoint(&mut self, endpoint: Endpoint) -> Result<()>;
    fn put_link(&mut self, link: Link) -> Result<()>;
    fn put_operation(&mut self, operation: Operation) -> Result<()>;
    fn append_operation_log(&mut self, record: OperationLogRecord) -> Result<()>;
    fn put_topology(&mut self, topology: Topology) -> Result<()>;
    fn put_log_view(&mut self, log_view: LogView) -> Result<()>;
    fn put_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()>;
    fn delete_service(&mut self, service_id: &str) -> Result<()>;
    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()>;
    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()>;
    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()>;

    fn service(&self, service_id: &str) -> Option<&ServiceManifest>;
    fn set(&self, set_id: &str) -> Option<&ServiceSet>;
    fn endpoint(&self, endpoint: &str) -> Option<&Endpoint>;
    fn operation(&self, operation_id: &str) -> Option<&Operation>;
    fn topology(&self, root_endpoint: &str) -> Option<&Topology>;

    fn services(&self) -> Vec<ServiceManifest>;
    fn sets(&self) -> Vec<ServiceSet>;
    fn endpoints(&self) -> Vec<Endpoint>;
    fn links(&self) -> Vec<Link>;
    fn operations(&self) -> Vec<Operation>;
    fn operation_logs(&self, operation_id: &str) -> Vec<OperationLogRecord>;
    fn topologies(&self) -> Vec<Topology>;
    fn log_views(&self) -> Vec<LogView>;
    fn diagnostic_reports(&self) -> Vec<DiagnosticReport>;
}

#[derive(Debug, Default, Clone)]
pub struct MemoryOrchestratorStore {
    services: BTreeMap<String, ServiceManifest>,
    sets: BTreeMap<String, ServiceSet>,
    endpoints: BTreeMap<String, Endpoint>,
    links: BTreeMap<(String, String), Link>,
    operations: BTreeMap<String, Operation>,
    operation_logs: Vec<OperationLogRecord>,
    topologies: BTreeMap<String, Topology>,
    log_views: BTreeMap<String, LogView>,
    diagnostic_reports: BTreeMap<String, DiagnosticReport>,
}

impl MemoryOrchestratorStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl OrchestratorStore for MemoryOrchestratorStore {
    fn put_service(&mut self, service: ServiceManifest) -> Result<()> {
        self.services.insert(service.id.clone(), service);
        Ok(())
    }

    fn put_set(&mut self, set: ServiceSet) -> Result<()> {
        self.sets.insert(set.id.clone(), set);
        Ok(())
    }

    fn put_endpoint(&mut self, endpoint: Endpoint) -> Result<()> {
        validate_endpoint(&endpoint)?;
        if !self.services.contains_key(&endpoint.service_id) {
            return Err(OrchestratorError::Dependency(format!(
                "endpoint references missing service {}",
                endpoint.service_id
            )));
        }
        self.endpoints.insert(endpoint.endpoint.clone(), endpoint);
        Ok(())
    }

    fn put_link(&mut self, link: Link) -> Result<()> {
        let endpoints = self.endpoints();
        validate_link(&link, &endpoints)?;
        self.links.insert(
            (link.source_endpoint.clone(), link.target_endpoint.clone()),
            link,
        );
        Ok(())
    }

    fn put_operation(&mut self, operation: Operation) -> Result<()> {
        self.operations
            .insert(operation.operation_id.clone(), operation);
        Ok(())
    }

    fn append_operation_log(&mut self, record: OperationLogRecord) -> Result<()> {
        if !self.operations.contains_key(&record.operation_id) {
            return Err(OrchestratorError::Dependency(format!(
                "operation log references missing operation {}",
                record.operation_id
            )));
        }
        self.operation_logs.push(record);
        Ok(())
    }

    fn put_topology(&mut self, topology: Topology) -> Result<()> {
        validate_topology(&topology)?;
        self.topologies
            .insert(topology.root_endpoint.clone(), topology);
        Ok(())
    }

    fn put_log_view(&mut self, log_view: LogView) -> Result<()> {
        if !self.endpoints.contains_key(&log_view.endpoint) {
            return Err(OrchestratorError::Dependency(format!(
                "log view references missing endpoint {}",
                log_view.endpoint
            )));
        }
        self.log_views.insert(log_view.source_id.clone(), log_view);
        Ok(())
    }

    fn put_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()> {
        self.diagnostic_reports
            .insert(report.report_id.clone(), report);
        Ok(())
    }

    fn delete_service(&mut self, service_id: &str) -> Result<()> {
        self.services.remove(service_id);
        let removed_endpoints = self
            .endpoints
            .values()
            .filter(|endpoint| endpoint.service_id == service_id)
            .map(|endpoint| endpoint.endpoint.clone())
            .collect::<Vec<_>>();
        for endpoint in removed_endpoints {
            self.delete_endpoint(&endpoint)?;
        }
        Ok(())
    }

    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()> {
        self.endpoints.remove(endpoint);
        self.links
            .retain(|(source, target), _| source != endpoint && target != endpoint);
        self.log_views
            .retain(|_, log_view| log_view.endpoint != endpoint);
        Ok(())
    }

    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()> {
        self.links
            .remove(&(source_endpoint.to_string(), target_endpoint.to_string()));
        Ok(())
    }

    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()> {
        self.topologies.remove(root_endpoint);
        Ok(())
    }

    fn service(&self, service_id: &str) -> Option<&ServiceManifest> {
        self.services.get(service_id)
    }

    fn set(&self, set_id: &str) -> Option<&ServiceSet> {
        self.sets.get(set_id)
    }

    fn endpoint(&self, endpoint: &str) -> Option<&Endpoint> {
        self.endpoints.get(endpoint)
    }

    fn operation(&self, operation_id: &str) -> Option<&Operation> {
        self.operations.get(operation_id)
    }

    fn topology(&self, root_endpoint: &str) -> Option<&Topology> {
        self.topologies.get(root_endpoint)
    }

    fn services(&self) -> Vec<ServiceManifest> {
        self.services.values().cloned().collect()
    }

    fn sets(&self) -> Vec<ServiceSet> {
        self.sets.values().cloned().collect()
    }

    fn endpoints(&self) -> Vec<Endpoint> {
        self.endpoints.values().cloned().collect()
    }

    fn links(&self) -> Vec<Link> {
        self.links.values().cloned().collect()
    }

    fn operations(&self) -> Vec<Operation> {
        self.operations.values().cloned().collect()
    }

    fn operation_logs(&self, operation_id: &str) -> Vec<OperationLogRecord> {
        self.operation_logs
            .iter()
            .filter(|record| record.operation_id == operation_id)
            .cloned()
            .collect()
    }

    fn topologies(&self) -> Vec<Topology> {
        self.topologies.values().cloned().collect()
    }

    fn log_views(&self) -> Vec<LogView> {
        self.log_views.values().cloned().collect()
    }

    fn diagnostic_reports(&self) -> Vec<DiagnosticReport> {
        self.diagnostic_reports.values().cloned().collect()
    }
}

pub struct OperationExecutor<'a, S: OrchestratorStore> {
    store: &'a mut S,
}

impl<'a, S: OrchestratorStore> OperationExecutor<'a, S> {
    pub fn new(store: &'a mut S) -> Self {
        Self { store }
    }

    pub fn apply(&mut self, operation_id: &str) -> Result<Operation> {
        let operation = self
            .store
            .operation(operation_id)
            .cloned()
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

        let mut running = operation.clone();
        running.status = OperationStatus::Running;
        self.store.put_operation(running.clone())?;
        self.store.append_operation_log(operation_log_record(
            &running.operation_id,
            "info",
            format!("operation {} started", running.action),
        ))?;
        let changed_objects = self.apply_operation_mutation(&running)?;

        let mut succeeded = running;
        succeeded.status = OperationStatus::Succeeded;
        succeeded.result = serde_json::json!({
            "operation_id": succeeded.operation_id,
            "status": "SUCCEEDED",
            "started_at": "",
            "finished_at": "",
            "changed_objects": changed_objects,
            "topology_snapshot_id": serde_json::Value::Null,
        });
        self.store.put_operation(succeeded.clone())?;
        self.store.append_operation_log(operation_log_record(
            &succeeded.operation_id,
            "info",
            format!("operation {} succeeded", succeeded.action),
        ))?;
        Ok(succeeded)
    }

    pub fn rollback(&mut self, operation_id: &str) -> Result<Operation> {
        let operation = self
            .store
            .operation(operation_id)
            .cloned()
            .ok_or_else(|| OrchestratorError::Dependency("operation not found".to_string()))?;
        if !matches!(
            operation.status,
            OperationStatus::Failed | OperationStatus::Succeeded
        ) {
            return Err(OrchestratorError::Blocked(format!(
                "operation status {:?} cannot rollback",
                operation.status
            )));
        }
        if operation.rollback_plan.is_null() {
            return Err(OrchestratorError::Blocked(
                "operation rollback plan is not available".to_string(),
            ));
        }
        let changed_objects = self.rollback_operation_mutation(&operation)?;

        let mut rolled_back = operation;
        rolled_back.status = OperationStatus::RolledBack;
        rolled_back.result = serde_json::json!({
            "operation_id": rolled_back.operation_id,
            "status": "ROLLED_BACK",
            "started_at": "",
            "finished_at": "",
            "changed_objects": changed_objects,
            "topology_snapshot_id": serde_json::Value::Null,
        });
        self.store.put_operation(rolled_back.clone())?;
        self.store.append_operation_log(operation_log_record(
            &rolled_back.operation_id,
            "info",
            format!("operation {} rolled back", rolled_back.action),
        ))?;
        Ok(rolled_back)
    }

    fn apply_operation_mutation(
        &mut self,
        operation: &Operation,
    ) -> Result<Vec<serde_json::Value>> {
        let mut changed = Vec::new();
        match operation.action.as_str() {
            "service.install" | "service.import" => {
                let service: ServiceManifest = request_value(operation, "service_manifest")?;
                self.store.put_service(service.clone())?;
                changed.push(changed_object("Service", &service.id));
            }
            "service.enable" | "service.disable" | "service.start" | "service.stop"
            | "service.restart" => {
                ensure_service_exists(self.store, operation.target_id.as_str())?;
                changed.push(changed_object("Service", &operation.target_id));
            }
            "service.delete" => {
                ensure_service_exists(self.store, operation.target_id.as_str())?;
                self.store.delete_service(&operation.target_id)?;
                changed.push(changed_object("Service", &operation.target_id));
            }
            "service.logs.view" => {
                if let Some(log_view) = log_view_from_operation(operation) {
                    self.store.put_log_view(log_view.clone())?;
                    changed.push(changed_object("LogView", &log_view.source_id));
                }
            }
            "service.health.check" => {
                if let Some(endpoint_id) = operation
                    .request
                    .get("endpoint")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    if let Some(mut endpoint) = self.store.endpoint(endpoint_id).cloned() {
                        endpoint.health = "ok".to_string();
                        endpoint.reachable = true;
                        self.store.put_endpoint(endpoint)?;
                        changed.push(changed_object("Endpoint", endpoint_id));
                    }
                }
            }
            "set.import" | "set.apply" => {
                let set: ServiceSet = request_value(operation, "set_manifest")?;
                self.store.put_set(set.clone())?;
                changed.push(changed_object("Set", &set.id));
                if operation.action == "set.apply" {
                    self.apply_set_defaults(&set, &mut changed)?;
                }
            }
            "endpoint.register" | "endpoint.update" => {
                let endpoint = endpoint_from_operation(operation, self.store)?;
                let endpoint_id = endpoint.endpoint.clone();
                self.store.put_endpoint(endpoint)?;
                changed.push(changed_object("Endpoint", &endpoint_id));
            }
            "endpoint.delete" => {
                self.store.delete_endpoint(&operation.target_id)?;
                changed.push(changed_object("Endpoint", &operation.target_id));
            }
            "endpoint.health.check" => {
                let endpoint_id = operation.target_id.as_str();
                let mut endpoint = self.store.endpoint(endpoint_id).cloned().ok_or_else(|| {
                    OrchestratorError::Dependency(format!("endpoint {endpoint_id} not found"))
                })?;
                endpoint.health = "ok".to_string();
                endpoint.reachable = true;
                self.store.put_endpoint(endpoint)?;
                changed.push(changed_object("Endpoint", endpoint_id));
            }
            "link.create" | "link.update" => {
                let link = link_from_operation(operation);
                let target = link_target_id(&link);
                self.store.put_link(link)?;
                changed.push(changed_object("Link", &target));
            }
            "link.delete" => {
                let link = link_from_operation(operation);
                self.store
                    .delete_link(&link.source_endpoint, &link.target_endpoint)?;
                changed.push(changed_object("Link", &link_target_id(&link)));
            }
            "link.health.check" => {
                let link = link_from_operation(operation);
                let target = link_target_id(&link);
                let mut links = self.store.links();
                let current = links.iter_mut().find(|item| {
                    item.source_endpoint == link.source_endpoint
                        && item.target_endpoint == link.target_endpoint
                });
                if let Some(current) = current {
                    current.health = "ok".to_string();
                    current.latency_ms = Some(0);
                    self.store.put_link(current.clone())?;
                    changed.push(changed_object("Link", &target));
                } else {
                    return Err(OrchestratorError::Dependency(format!(
                        "link {target} not found"
                    )));
                }
            }
            "topology.apply" => {
                let topology: Topology = request_value(operation, "topology_snapshot")?;
                self.store.put_topology(topology.clone())?;
                changed.push(changed_object("Topology", &topology.root_endpoint));
                for log_view in &topology.log_views {
                    self.store.put_log_view(log_view.clone())?;
                    changed.push(changed_object("LogView", &log_view.source_id));
                }
                for report in &topology.diagnostic_reports {
                    self.store.put_diagnostic_report(report.clone())?;
                    changed.push(changed_object("DiagnosticReport", &report.report_id));
                }
            }
            "diagnostics.run" => {
                let report = DiagnosticReport {
                    report_id: format!("diag-{}", operation.operation_id),
                    target_type: operation.target_type.clone(),
                    target_id: operation.target_id.clone(),
                    status: "ok".to_string(),
                    summary: format!("{} 诊断完成", operation.target_id),
                    findings: Vec::new(),
                    created_at: String::new(),
                };
                self.store.put_diagnostic_report(report.clone())?;
                changed.push(changed_object("DiagnosticReport", &report.report_id));
            }
            _ => {
                changed.push(changed_object(&operation.target_type, &operation.target_id));
            }
        }
        Ok(changed)
    }

    fn rollback_operation_mutation(
        &mut self,
        operation: &Operation,
    ) -> Result<Vec<serde_json::Value>> {
        let mut changed = Vec::new();
        match operation.action.as_str() {
            "service.install" | "service.import" => {
                let already_known = operation
                    .request
                    .get("already_known")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if !already_known {
                    self.store.delete_service(&operation.target_id)?;
                    changed.push(changed_object("Service", &operation.target_id));
                }
            }
            "endpoint.register" => {
                self.store.delete_endpoint(&operation.target_id)?;
                changed.push(changed_object("Endpoint", &operation.target_id));
            }
            "link.create" => {
                let link = link_from_operation(operation);
                self.store
                    .delete_link(&link.source_endpoint, &link.target_endpoint)?;
                changed.push(changed_object("Link", &link_target_id(&link)));
            }
            "topology.apply" => {
                self.store.delete_topology(&operation.target_id)?;
                changed.push(changed_object("Topology", &operation.target_id));
            }
            "service.delete" | "endpoint.delete" | "link.delete" => {
                changed.push(changed_object(&operation.target_type, &operation.target_id));
            }
            _ => {
                changed.push(changed_object(&operation.target_type, &operation.target_id));
            }
        }
        Ok(changed)
    }

    fn apply_set_defaults(
        &mut self,
        set: &ServiceSet,
        changed: &mut Vec<serde_json::Value>,
    ) -> Result<()> {
        for endpoint in &set.default_endpoints {
            ensure_service_exists(self.store, &endpoint.service)?;
            let endpoint_id = format!("127.0.0.1:{}", endpoint.port);
            self.store.put_endpoint(Endpoint {
                endpoint: endpoint_id.clone(),
                service_id: endpoint.service.clone(),
                protocol: endpoint.protocol.clone(),
                health_path: String::new(),
                health: "unknown".to_string(),
                reachable: false,
                display_name: format!("{} default endpoint", endpoint.service),
                note: format!("由 Set {} 应用生成", set.id),
                config: serde_json::json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            })?;
            changed.push(changed_object("Endpoint", &endpoint_id));
        }
        for link in &set.default_links {
            let Some(source) = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.from)
                .map(|endpoint| format!("127.0.0.1:{}", endpoint.port))
            else {
                continue;
            };
            let Some(target) = set
                .default_endpoints
                .iter()
                .find(|endpoint| endpoint.service == link.to)
                .map(|endpoint| format!("127.0.0.1:{}", endpoint.port))
            else {
                continue;
            };
            let link_model = Link {
                source_endpoint: source,
                target_endpoint: target,
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
            };
            let target = link_target_id(&link_model);
            self.store.put_link(link_model)?;
            changed.push(changed_object("Link", &target));
        }
        Ok(())
    }
}

fn request_value<T: DeserializeOwned>(operation: &Operation, field: &str) -> Result<T> {
    let value = operation.request.get(field).cloned().ok_or_else(|| {
        OrchestratorError::Dependency(format!(
            "operation {} request missing {field}",
            operation.operation_id
        ))
    })?;
    serde_json::from_value(value).map_err(OrchestratorError::Json)
}

fn endpoint_from_operation<S: OrchestratorStore>(
    operation: &Operation,
    store: &S,
) -> Result<Endpoint> {
    let endpoint_id = operation
        .request
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(operation.target_id.as_str());
    let current = store.endpoint(endpoint_id).cloned();
    Ok(Endpoint {
        endpoint: endpoint_id.to_string(),
        service_id: operation
            .request
            .get("service_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                current
                    .as_ref()
                    .map(|endpoint| endpoint.service_id.as_str())
            })
            .unwrap_or("")
            .to_string(),
        protocol: operation
            .request
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .or_else(|| current.as_ref().map(|endpoint| endpoint.protocol.as_str()))
            .unwrap_or("http")
            .to_string(),
        health_path: operation
            .request
            .get("health_path")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                current
                    .as_ref()
                    .map(|endpoint| endpoint.health_path.as_str())
            })
            .unwrap_or("")
            .to_string(),
        health: current
            .as_ref()
            .map(|endpoint| endpoint.health.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        reachable: current.as_ref().is_some_and(|endpoint| endpoint.reachable),
        display_name: operation
            .request
            .get("display_name")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                current
                    .as_ref()
                    .map(|endpoint| endpoint.display_name.as_str())
            })
            .unwrap_or("")
            .to_string(),
        note: operation
            .request
            .get("note")
            .and_then(serde_json::Value::as_str)
            .or_else(|| current.as_ref().map(|endpoint| endpoint.note.as_str()))
            .unwrap_or("")
            .to_string(),
        config: current
            .as_ref()
            .map(|endpoint| endpoint.config.clone())
            .unwrap_or_else(|| serde_json::json!({})),
        created_at: current
            .as_ref()
            .map(|endpoint| endpoint.created_at.clone())
            .unwrap_or_default(),
        updated_at: String::new(),
    })
}

fn link_from_operation(operation: &Operation) -> Link {
    Link {
        source_endpoint: operation
            .request
            .get("source_endpoint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        target_endpoint: operation
            .request
            .get("target_endpoint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        protocol: operation
            .request
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("http")
            .to_string(),
        auth_mode: operation
            .request
            .get("auth_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("internal")
            .to_string(),
        scope: operation
            .request
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: operation
            .request
            .get("config_ref")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        secret_ref: operation
            .request
            .get("secret_ref")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn log_view_from_operation(operation: &Operation) -> Option<LogView> {
    let endpoint = operation
        .request
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())?;
    let service_id = operation
        .request
        .get("service_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(operation.target_id.as_str());
    Some(LogView {
        source_id: format!("{service_id}:{endpoint}"),
        service_id: service_id.to_string(),
        endpoint: endpoint.to_string(),
        location: "/logs".to_string(),
        display_name: format!("{service_id} logs"),
    })
}

fn ensure_service_exists<S: OrchestratorStore>(store: &S, service_id: &str) -> Result<()> {
    if store.service(service_id).is_some() {
        Ok(())
    } else {
        Err(OrchestratorError::Dependency(format!(
            "service {service_id} not found"
        )))
    }
}

fn changed_object(object_type: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": object_type,
        "id": id
    })
}

fn link_target_id(link: &Link) -> String {
    format!("{} -> {}", link.source_endpoint, link.target_endpoint)
}

fn empty_to_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}
