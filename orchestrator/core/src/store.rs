use crate::{
    DiagnosticReport, DockerComposeDriver, DriverRequest, DriverResult, Endpoint,
    EndpointHealthResult, EndpointProbe, ExecutionDriver, ExternalEndpointDriver, Link,
    LinkHealthResult, LocalProcessDriver, LogView, Operation, OperationLock, OperationLogRecord,
    OperationStatus, OrchestratorError, Result, RuntimeMode, ServiceManifest, ServiceSet,
    StaticEndpointProbe, Topology, TopologySnapshot, build_diagnostic_report, build_topology,
    check_endpoint_health_with_probe, check_link_health, export_diagnostic_report,
    operation_log_record, operation_step_log_record, start_operation, succeed_operation,
    validate_endpoint, validate_link, validate_log_view, validate_topology,
};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

pub trait OrchestratorStore {
    fn list_services(&self) -> Result<Vec<ServiceManifest>>;
    fn get_service(&self, service_id: &str) -> Result<Option<ServiceManifest>>;
    fn upsert_service(&mut self, service: ServiceManifest) -> Result<()>;
    fn delete_service(&mut self, service_id: &str) -> Result<()>;

    fn list_sets(&self) -> Result<Vec<ServiceSet>>;
    fn get_set(&self, set_id: &str) -> Result<Option<ServiceSet>>;
    fn upsert_set(&mut self, set: ServiceSet) -> Result<()>;
    fn delete_set(&mut self, set_id: &str) -> Result<()>;

    fn list_endpoints(&self) -> Result<Vec<Endpoint>>;
    fn get_endpoint(&self, endpoint: &str) -> Result<Option<Endpoint>>;
    fn upsert_endpoint(&mut self, endpoint: Endpoint) -> Result<()>;
    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()>;
    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> Result<()>;

    fn list_links(&self) -> Result<Vec<Link>>;
    fn get_link(&self, source_endpoint: &str, target_endpoint: &str) -> Result<Option<Link>>;
    fn upsert_link(&mut self, link: Link) -> Result<()>;
    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()>;
    fn update_link_health(
        &mut self,
        source_endpoint: &str,
        target_endpoint: &str,
        health: String,
        latency_ms: Option<u32>,
    ) -> Result<()>;

    fn create_operation(&mut self, operation: Operation) -> Result<()>;
    fn get_operation(&self, operation_id: &str) -> Result<Option<Operation>>;
    fn list_operations(&self) -> Result<Vec<Operation>>;
    fn update_operation(&mut self, operation: Operation) -> Result<()>;
    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> Result<()>;
    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> Result<()>;
    fn append_operation_log(&mut self, record: OperationLogRecord) -> Result<()>;
    fn list_operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>>;
    fn acquire_operation_lock(&mut self, lock: OperationLock) -> Result<bool>;
    fn release_operation_lock(&mut self, lock_key: &str, operation_id: &str) -> Result<()>;

    fn save_topology_snapshot(&mut self, snapshot: TopologySnapshot) -> Result<()>;
    fn get_latest_topology_snapshot(&self) -> Result<Option<TopologySnapshot>>;
    fn build_topology_view(&self) -> Result<Topology>;
    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()>;

    fn list_log_sources(&self) -> Result<Vec<LogView>>;
    fn upsert_log_source(&mut self, log_view: LogView) -> Result<()>;
    fn delete_log_source(&mut self, source_id: &str) -> Result<()>;

    fn create_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()>;
    fn get_diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>>;
    fn list_diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>>;

    fn put_service(&mut self, service: ServiceManifest) -> Result<()> {
        self.upsert_service(service)
    }

    fn put_set(&mut self, set: ServiceSet) -> Result<()> {
        self.upsert_set(set)
    }

    fn put_endpoint(&mut self, endpoint: Endpoint) -> Result<()> {
        self.upsert_endpoint(endpoint)
    }

    fn put_link(&mut self, link: Link) -> Result<()> {
        self.upsert_link(link)
    }

    fn put_operation(&mut self, operation: Operation) -> Result<()> {
        self.update_operation(operation)
    }

    fn put_topology(&mut self, topology: Topology) -> Result<()> {
        self.save_topology_snapshot(TopologySnapshot {
            snapshot_id: topology.root_endpoint.clone(),
            topology,
            created_at: String::new(),
        })
    }

    fn put_log_view(&mut self, log_view: LogView) -> Result<()> {
        self.upsert_log_source(log_view)
    }

    fn put_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()> {
        self.create_diagnostic_report(report)
    }

    fn services(&self) -> Result<Vec<ServiceManifest>> {
        self.list_services()
    }

    fn sets(&self) -> Result<Vec<ServiceSet>> {
        self.list_sets()
    }

    fn endpoints(&self) -> Result<Vec<Endpoint>> {
        self.list_endpoints()
    }

    fn links(&self) -> Result<Vec<Link>> {
        self.list_links()
    }

    fn operations(&self) -> Result<Vec<Operation>> {
        self.list_operations()
    }

    fn operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        self.list_operation_logs(operation_id)
    }

    fn log_views(&self) -> Result<Vec<LogView>> {
        self.list_log_sources()
    }

    fn diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        self.list_diagnostic_reports()
    }
}

#[derive(Debug, Default, Clone)]
pub struct MemoryOrchestratorStore {
    services: BTreeMap<String, ServiceManifest>,
    sets: BTreeMap<String, ServiceSet>,
    endpoints: BTreeMap<String, Endpoint>,
    links: BTreeMap<(String, String), Link>,
    operations: BTreeMap<String, Operation>,
    operation_logs: Vec<OperationLogRecord>,
    topology_snapshots: BTreeMap<String, TopologySnapshot>,
    log_views: BTreeMap<String, LogView>,
    diagnostic_reports: BTreeMap<String, DiagnosticReport>,
    operation_locks: BTreeMap<String, OperationLock>,
}

impl MemoryOrchestratorStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn service(&self, service_id: &str) -> Option<&ServiceManifest> {
        self.services.get(service_id)
    }

    pub fn set(&self, set_id: &str) -> Option<&ServiceSet> {
        self.sets.get(set_id)
    }

    pub fn endpoint(&self, endpoint: &str) -> Option<&Endpoint> {
        self.endpoints.get(endpoint)
    }

    pub fn operation(&self, operation_id: &str) -> Option<&Operation> {
        self.operations.get(operation_id)
    }

    pub fn topology(&self, root_endpoint: &str) -> Option<&Topology> {
        self.topology_snapshots
            .values()
            .find(|snapshot| snapshot.topology.root_endpoint == root_endpoint)
            .map(|snapshot| &snapshot.topology)
    }

    pub fn services(&self) -> Vec<ServiceManifest> {
        self.services.values().cloned().collect()
    }

    pub fn sets(&self) -> Vec<ServiceSet> {
        self.sets.values().cloned().collect()
    }

    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.endpoints.values().cloned().collect()
    }

    pub fn links(&self) -> Vec<Link> {
        self.links.values().cloned().collect()
    }

    pub fn operations(&self) -> Vec<Operation> {
        self.operations.values().cloned().collect()
    }

    pub fn operation_logs(&self, operation_id: &str) -> Vec<OperationLogRecord> {
        self.operation_logs
            .iter()
            .filter(|record| record.operation_id == operation_id)
            .cloned()
            .collect()
    }

    pub fn topologies(&self) -> Vec<Topology> {
        self.topology_snapshots
            .values()
            .map(|snapshot| snapshot.topology.clone())
            .collect()
    }

    pub fn log_views(&self) -> Vec<LogView> {
        self.log_views.values().cloned().collect()
    }

    pub fn diagnostic_reports(&self) -> Vec<DiagnosticReport> {
        self.diagnostic_reports.values().cloned().collect()
    }
}

impl OrchestratorStore for MemoryOrchestratorStore {
    fn list_services(&self) -> Result<Vec<ServiceManifest>> {
        Ok(self.services())
    }

    fn get_service(&self, service_id: &str) -> Result<Option<ServiceManifest>> {
        Ok(self.services.get(service_id).cloned())
    }

    fn upsert_service(&mut self, service: ServiceManifest) -> Result<()> {
        self.services.insert(service.id.clone(), service);
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

    fn list_sets(&self) -> Result<Vec<ServiceSet>> {
        Ok(self.sets())
    }

    fn get_set(&self, set_id: &str) -> Result<Option<ServiceSet>> {
        Ok(self.sets.get(set_id).cloned())
    }

    fn upsert_set(&mut self, set: ServiceSet) -> Result<()> {
        self.sets.insert(set.id.clone(), set);
        Ok(())
    }

    fn delete_set(&mut self, set_id: &str) -> Result<()> {
        self.sets.remove(set_id);
        Ok(())
    }

    fn list_endpoints(&self) -> Result<Vec<Endpoint>> {
        Ok(self.endpoints())
    }

    fn get_endpoint(&self, endpoint: &str) -> Result<Option<Endpoint>> {
        Ok(self.endpoints.get(endpoint).cloned())
    }

    fn upsert_endpoint(&mut self, endpoint: Endpoint) -> Result<()> {
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

    fn delete_endpoint(&mut self, endpoint: &str) -> Result<()> {
        self.endpoints.remove(endpoint);
        self.links
            .retain(|(source, target), _| source != endpoint && target != endpoint);
        self.log_views
            .retain(|_, log_view| log_view.endpoint != endpoint);
        Ok(())
    }

    fn update_endpoint_health(
        &mut self,
        endpoint: &str,
        health: String,
        reachable: bool,
    ) -> Result<()> {
        let item = self.endpoints.get_mut(endpoint).ok_or_else(|| {
            OrchestratorError::Dependency(format!("endpoint {endpoint} not found"))
        })?;
        item.health = health;
        item.reachable = reachable;
        Ok(())
    }

    fn list_links(&self) -> Result<Vec<Link>> {
        Ok(self.links())
    }

    fn get_link(&self, source_endpoint: &str, target_endpoint: &str) -> Result<Option<Link>> {
        Ok(self
            .links
            .get(&(source_endpoint.to_string(), target_endpoint.to_string()))
            .cloned())
    }

    fn upsert_link(&mut self, link: Link) -> Result<()> {
        let endpoints = self.endpoints();
        validate_link(&link, &endpoints)?;
        self.links.insert(
            (link.source_endpoint.clone(), link.target_endpoint.clone()),
            link,
        );
        Ok(())
    }

    fn delete_link(&mut self, source_endpoint: &str, target_endpoint: &str) -> Result<()> {
        self.links
            .remove(&(source_endpoint.to_string(), target_endpoint.to_string()))
            .map(|_| ())
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "link {source_endpoint} -> {target_endpoint} not found"
                ))
            })
    }

    fn update_link_health(
        &mut self,
        source_endpoint: &str,
        target_endpoint: &str,
        health: String,
        latency_ms: Option<u32>,
    ) -> Result<()> {
        let item = self
            .links
            .get_mut(&(source_endpoint.to_string(), target_endpoint.to_string()))
            .ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "link {source_endpoint} -> {target_endpoint} not found"
                ))
            })?;
        item.health = health;
        item.latency_ms = latency_ms;
        Ok(())
    }

    fn create_operation(&mut self, operation: Operation) -> Result<()> {
        self.operations
            .insert(operation.operation_id.clone(), operation);
        Ok(())
    }

    fn get_operation(&self, operation_id: &str) -> Result<Option<Operation>> {
        Ok(self.operations.get(operation_id).cloned())
    }

    fn list_operations(&self) -> Result<Vec<Operation>> {
        Ok(self.operations())
    }

    fn update_operation(&mut self, operation: Operation) -> Result<()> {
        self.operations
            .insert(operation.operation_id.clone(), operation);
        Ok(())
    }

    fn update_operation_status(
        &mut self,
        operation_id: &str,
        status: OperationStatus,
        error_message: String,
    ) -> Result<()> {
        let operation = self.operations.get_mut(operation_id).ok_or_else(|| {
            OrchestratorError::Dependency(format!("operation {operation_id} not found"))
        })?;
        operation.status = status;
        operation.error_message = error_message;
        Ok(())
    }

    fn update_operation_result(
        &mut self,
        operation_id: &str,
        result: serde_json::Value,
    ) -> Result<()> {
        let operation = self.operations.get_mut(operation_id).ok_or_else(|| {
            OrchestratorError::Dependency(format!("operation {operation_id} not found"))
        })?;
        operation.result = result;
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

    fn list_operation_logs(&self, operation_id: &str) -> Result<Vec<OperationLogRecord>> {
        Ok(self.operation_logs(operation_id))
    }

    fn acquire_operation_lock(&mut self, lock: OperationLock) -> Result<bool> {
        if !self.operations.contains_key(&lock.operation_id) {
            return Err(OrchestratorError::Dependency(format!(
                "lock references missing operation {}",
                lock.operation_id
            )));
        }
        if self.operation_locks.contains_key(&lock.lock_key) {
            return Ok(false);
        }
        self.operation_locks.insert(lock.lock_key.clone(), lock);
        Ok(true)
    }

    fn release_operation_lock(&mut self, lock_key: &str, operation_id: &str) -> Result<()> {
        if self
            .operation_locks
            .get(lock_key)
            .is_some_and(|lock| lock.operation_id == operation_id)
        {
            self.operation_locks.remove(lock_key);
        }
        Ok(())
    }

    fn save_topology_snapshot(&mut self, snapshot: TopologySnapshot) -> Result<()> {
        validate_topology(&snapshot.topology)?;
        self.topology_snapshots
            .insert(snapshot.snapshot_id.clone(), snapshot);
        Ok(())
    }

    fn get_latest_topology_snapshot(&self) -> Result<Option<TopologySnapshot>> {
        Ok(self.topology_snapshots.values().last().cloned())
    }

    fn build_topology_view(&self) -> Result<Topology> {
        if let Some(snapshot) = self.get_latest_topology_snapshot()? {
            return Ok(snapshot.topology);
        }
        let endpoints = self.endpoints();
        let root_endpoint = endpoints
            .iter()
            .find(|endpoint| endpoint.service_id == "gateway")
            .or_else(|| endpoints.first())
            .map(|endpoint| endpoint.endpoint.clone())
            .ok_or_else(|| OrchestratorError::Dependency("no endpoint for topology".to_string()))?;
        build_topology(
            root_endpoint,
            self.services.keys().cloned().collect(),
            self.sets.keys().cloned().collect(),
            endpoints,
            self.links(),
            self.operations(),
            self.log_views(),
            self.diagnostic_reports(),
        )
    }

    fn delete_topology(&mut self, root_endpoint: &str) -> Result<()> {
        self.topology_snapshots
            .retain(|_, snapshot| snapshot.topology.root_endpoint != root_endpoint);
        Ok(())
    }

    fn list_log_sources(&self) -> Result<Vec<LogView>> {
        Ok(self.log_views())
    }

    fn upsert_log_source(&mut self, log_view: LogView) -> Result<()> {
        validate_log_view(&log_view)?;
        if !self.endpoints.contains_key(&log_view.endpoint) {
            return Err(OrchestratorError::Dependency(format!(
                "log view references missing endpoint {}",
                log_view.endpoint
            )));
        }
        self.log_views.insert(log_view.source_id.clone(), log_view);
        Ok(())
    }

    fn delete_log_source(&mut self, source_id: &str) -> Result<()> {
        self.log_views.remove(source_id);
        Ok(())
    }

    fn create_diagnostic_report(&mut self, report: DiagnosticReport) -> Result<()> {
        self.diagnostic_reports
            .insert(report.report_id.clone(), report);
        Ok(())
    }

    fn get_diagnostic_report(&self, report_id: &str) -> Result<Option<DiagnosticReport>> {
        Ok(self.diagnostic_reports.get(report_id).cloned())
    }

    fn list_diagnostic_reports(&self) -> Result<Vec<DiagnosticReport>> {
        Ok(self.diagnostic_reports())
    }
}

pub struct OperationExecutor<'a, S: OrchestratorStore, P: EndpointProbe = StaticEndpointProbe> {
    store: &'a mut S,
    endpoint_probe: P,
}

impl<'a, S: OrchestratorStore> OperationExecutor<'a, S, StaticEndpointProbe> {
    pub fn new(store: &'a mut S) -> Self {
        Self {
            store,
            endpoint_probe: StaticEndpointProbe,
        }
    }
}

impl<'a, S: OrchestratorStore, P: EndpointProbe> OperationExecutor<'a, S, P> {
    pub fn with_endpoint_probe(store: &'a mut S, endpoint_probe: P) -> Self {
        Self {
            store,
            endpoint_probe,
        }
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

        let result = match self.apply_operation_mutation(&running) {
            Ok(changed_objects) => {
                let result = serde_json::json!({
                    "operation_id": running.operation_id,
                    "status": "SUCCEEDED",
                    "started_at": running.started_at,
                    "finished_at": "finished",
                    "changed_objects": changed_objects,
                    "topology_snapshot_id": serde_json::Value::Null,
                });
                let succeeded = succeed_operation(&running, result)?;
                self.store.update_operation(succeeded.clone())?;
                self.store.append_operation_log(operation_log_record(
                    &succeeded.operation_id,
                    "info",
                    format!("operation {} succeeded", succeeded.action),
                ))?;
                Ok(succeeded)
            }
            Err(err) => {
                let failed = crate::fail_operation(&running, err.to_string())?;
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
        };
        result
    }

    pub fn rollback(&mut self, operation_id: &str) -> Result<Operation> {
        let operation = self
            .store
            .get_operation(operation_id)?
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

        let result = self.rollback_with_acquired_lock(&operation);
        self.store.release_operation_lock(&lock_key, operation_id)?;
        result
    }

    fn rollback_with_acquired_lock(&mut self, operation: &Operation) -> Result<Operation> {
        let operation_id = operation.operation_id.as_str();
        let prior_logs = self.store.list_operation_logs(operation_id)?;
        self.store.append_operation_log(operation_log_record(
            &operation.operation_id,
            "info",
            format!("rollback loaded {} prior operation logs", prior_logs.len()),
        ))?;
        for (index, step) in rollback_steps(&operation).iter().enumerate() {
            self.store.append_operation_log(operation_step_log_record(
                &operation.operation_id,
                format!("rollback:{}", step_id(step, index)),
                "info",
                format!("rollback step {} planned", step_label(step)),
                step.clone(),
            ))?;
        }
        let changed_objects = match self.rollback_operation_mutation(&operation) {
            Ok(changed_objects) => changed_objects,
            Err(err) => {
                self.store.append_operation_log(operation_log_record(
                    &operation.operation_id,
                    "error",
                    format!("operation {} rollback failed: {err}", operation.action),
                ))?;
                return Err(err);
            }
        };

        let result = serde_json::json!({
            "operation_id": operation.operation_id,
            "status": "ROLLED_BACK",
            "started_at": operation.started_at,
            "finished_at": "rolled_back",
            "changed_objects": changed_objects,
            "topology_snapshot_id": serde_json::Value::Null,
        });
        let rolled_back = crate::rollback_operation(&operation, result)?;
        self.store.update_operation(rolled_back.clone())?;
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
                let service = ensure_service_exists(self.store, operation.target_id.as_str())?;
                let driver_result = execute_service_driver_action(&service, operation)?;
                self.store.append_operation_log(driver_result_log_record(
                    &operation.operation_id,
                    &driver_result,
                ))?;
                changed.push(changed_object("Service", &operation.target_id));
            }
            "service.delete" => {
                let service = ensure_service_exists(self.store, operation.target_id.as_str())?;
                let driver_result = execute_service_driver_action(&service, operation)?;
                self.store.append_operation_log(driver_result_log_record(
                    &operation.operation_id,
                    &driver_result,
                ))?;
                self.store.delete_service(&operation.target_id)?;
                changed.push(changed_object("Service", &operation.target_id));
            }
            "service.logs.view" => {
                if let Some(log_view) = log_view_from_operation(operation) {
                    self.store.put_log_view(log_view.clone())?;
                    self.store.append_operation_log(log_view_log_record(
                        &operation.operation_id,
                        &log_view,
                    ))?;
                    changed.push(changed_object("LogView", &log_view.source_id));
                }
            }
            "service.health.check" => {
                ensure_service_exists(self.store, operation.target_id.as_str())?;
                let requested_endpoint = operation
                    .request
                    .get("endpoint")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty());
                if let Some(endpoint_id) = requested_endpoint {
                    let endpoint = self.store.get_endpoint(endpoint_id)?.ok_or_else(|| {
                        OrchestratorError::Dependency(format!("endpoint {endpoint_id} not found"))
                    })?;
                    if endpoint.service_id != operation.target_id {
                        return Err(OrchestratorError::Dependency(format!(
                            "endpoint {endpoint_id} does not belong to service {}",
                            operation.target_id
                        )));
                    }
                    self.probe_endpoint_and_persist(&operation.operation_id, &endpoint)?;
                    changed.push(changed_object("Endpoint", endpoint_id));
                } else {
                    let endpoints = self
                        .store
                        .list_endpoints()?
                        .into_iter()
                        .filter(|endpoint| endpoint.service_id == operation.target_id)
                        .collect::<Vec<_>>();
                    if endpoints.is_empty() {
                        return Err(OrchestratorError::Dependency(format!(
                            "service {} has no registered endpoints",
                            operation.target_id
                        )));
                    }
                    for endpoint in endpoints {
                        self.probe_endpoint_and_persist(&operation.operation_id, &endpoint)?;
                        changed.push(changed_object("Endpoint", &endpoint.endpoint));
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
                let endpoint = self.store.get_endpoint(endpoint_id)?.ok_or_else(|| {
                    OrchestratorError::Dependency(format!("endpoint {endpoint_id} not found"))
                })?;
                self.probe_endpoint_and_persist(&operation.operation_id, &endpoint)?;
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
                let requested = link_from_operation(operation);
                let link = self
                    .store
                    .get_link(&requested.source_endpoint, &requested.target_endpoint)?
                    .ok_or_else(|| {
                        OrchestratorError::Dependency(format!(
                            "link {} not found",
                            link_target_id(&requested)
                        ))
                    })?;
                let target = link_target_id(&link);
                let endpoints = self.store.list_endpoints()?;
                let target_health = if let Some(endpoint) = endpoints
                    .iter()
                    .find(|endpoint| endpoint.endpoint == link.target_endpoint)
                {
                    self.probe_endpoint_and_persist(&operation.operation_id, endpoint)?
                } else {
                    missing_target_health(&link)
                };
                let link_health = check_link_health(&link, &endpoints, &target_health)?;
                self.store.update_link_health(
                    &link.source_endpoint,
                    &link.target_endpoint,
                    link_health.health.clone(),
                    link_health.latency_ms,
                )?;
                self.store.append_operation_log(link_health_log_record(
                    &operation.operation_id,
                    &link_health,
                ))?;
                changed.push(changed_object("Link", &target));
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
                let mut report = build_diagnostic_report(
                    self.store,
                    format!("diag-{}", operation.operation_id),
                )?;
                report.operation_id = operation.operation_id.clone();
                self.store.put_diagnostic_report(report.clone())?;
                changed.push(changed_object("DiagnosticReport", &report.report_id));
            }
            "operation.logs.view" => {
                let target_operation_id = operation
                    .request
                    .get("operation_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let target_operation =
                    self.store
                        .get_operation(target_operation_id)?
                        .ok_or_else(|| {
                            OrchestratorError::Dependency(format!(
                                "operation {target_operation_id} not found"
                            ))
                        })?;
                let target_logs = self.store.list_operation_logs(target_operation_id)?;
                let endpoints = self.store.list_endpoints()?;
                let log_view = operation_log_view_from_target(&target_operation, &endpoints)?;
                self.store.put_log_view(log_view.clone())?;
                self.store.append_operation_log(operation_step_log_record(
                    &operation.operation_id,
                    "operation.logs.view",
                    "info",
                    format!(
                        "operation {} logs view opened with {} records",
                        target_operation_id,
                        target_logs.len()
                    ),
                    serde_json::json!({
                        "operation_id": target_operation_id,
                        "log_count": target_logs.len(),
                        "source_id": log_view.source_id,
                    }),
                ))?;
                changed.push(changed_object("LogView", &log_view.source_id));
            }
            "diagnostics.export" => {
                let report_id = operation
                    .request
                    .get("report_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(operation.target_id.as_str());
                let format = operation
                    .request
                    .get("format")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("json");
                let report = match self.store.get_diagnostic_report(report_id)? {
                    Some(report) => report,
                    None => {
                        let mut report = build_diagnostic_report(self.store, report_id)?;
                        report.operation_id = operation.operation_id.clone();
                        self.store.put_diagnostic_report(report.clone())?;
                        report
                    }
                };
                let export = export_diagnostic_report(&report, format)?;
                self.store.append_operation_log(operation_step_log_record(
                    &operation.operation_id,
                    "diagnostics.export",
                    "info",
                    format!(
                        "diagnostic report {} exported as {}",
                        export.report_id, export.format
                    ),
                    serde_json::json!({
                        "report_id": export.report_id,
                        "format": export.format,
                        "content_bytes": export.content.len(),
                    }),
                ))?;
                changed.push(changed_object("DiagnosticReport", report_id));
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

    fn probe_endpoint_and_persist(
        &mut self,
        operation_id: &str,
        endpoint: &Endpoint,
    ) -> Result<EndpointHealthResult> {
        let health = check_endpoint_health_with_probe(endpoint, &self.endpoint_probe)?;
        self.store.update_endpoint_health(
            &health.endpoint,
            health.health.clone(),
            health.reachable,
        )?;
        self.store
            .append_operation_log(endpoint_health_log_record(operation_id, &health))?;
        Ok(health)
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
    let current = store.get_endpoint(endpoint_id)?;
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
        operation_id: operation.operation_id.clone(),
        path: "/logs".to_string(),
        driver: "external-endpoint".to_string(),
        read_policy: "service-scoped".to_string(),
        display_name: format!("{service_id} logs"),
    })
}

fn operation_log_view_from_target(
    operation: &Operation,
    endpoints: &[Endpoint],
) -> Result<LogView> {
    let requested_endpoint = operation
        .request
        .get("endpoint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty());
    let endpoint = requested_endpoint
        .and_then(|value| {
            endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint == value)
                .map(|endpoint| endpoint.endpoint.clone())
        })
        .or_else(|| {
            endpoints
                .iter()
                .find(|endpoint| endpoint.service_id == operation.target_id)
                .map(|endpoint| endpoint.endpoint.clone())
        })
        .or_else(|| endpoints.first().map(|endpoint| endpoint.endpoint.clone()))
        .ok_or_else(|| {
            OrchestratorError::Dependency(
                "operation log view requires at least one registered endpoint".to_string(),
            )
        })?;
    Ok(LogView {
        source_id: format!("operation:{}", operation.operation_id),
        service_id: operation.target_id.clone(),
        endpoint,
        operation_id: operation.operation_id.clone(),
        path: "/operations/logs".to_string(),
        driver: "external-endpoint".to_string(),
        read_policy: "operation-scoped".to_string(),
        display_name: format!("{} logs", operation.operation_id),
    })
}

fn ensure_service_exists<S: OrchestratorStore>(
    store: &S,
    service_id: &str,
) -> Result<ServiceManifest> {
    store
        .get_service(service_id)?
        .ok_or_else(|| OrchestratorError::Dependency(format!("service {service_id} not found")))
}

fn execute_service_driver_action(
    service: &ServiceManifest,
    operation: &Operation,
) -> Result<DriverResult> {
    let request = DriverRequest {
        action: operation.action.clone(),
        service_id: service.id.clone(),
        endpoint: operation
            .request
            .get("endpoint")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        link: None,
        log_source: None,
    };
    match service.runtime.mode {
        RuntimeMode::Container => {
            DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml").execute(&request)
        }
        RuntimeMode::LocalProcess => LocalProcessDriver::new().execute(&request),
        RuntimeMode::External => ExternalEndpointDriver.execute(&request),
    }
}

fn driver_result_log_record(operation_id: &str, result: &DriverResult) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("driver:{}", result.action),
        "info",
        format!(
            "driver action {} returned {}: {}",
            result.action, result.status, result.message
        ),
        serde_json::json!({
            "action": result.action,
            "status": result.status,
            "message": result.message,
            "command": result.command,
        }),
    )
}

fn log_view_log_record(operation_id: &str, log_view: &LogView) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("log-view:{}", log_view.source_id),
        "info",
        format!("log view {} opened", log_view.source_id),
        serde_json::json!({
            "source_id": log_view.source_id,
            "service_id": log_view.service_id,
            "endpoint": log_view.endpoint,
            "operation_id": log_view.operation_id,
            "read_policy": log_view.read_policy,
        }),
    )
}

fn endpoint_health_log_record(
    operation_id: &str,
    result: &EndpointHealthResult,
) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!("health:endpoint:{}", result.endpoint),
        if result.reachable { "info" } else { "warn" },
        format!(
            "endpoint {} health {}: {}",
            result.endpoint, result.health, result.message
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

fn link_health_log_record(operation_id: &str, result: &LinkHealthResult) -> OperationLogRecord {
    operation_step_log_record(
        operation_id,
        format!(
            "health:link:{}->{}",
            result.source_endpoint, result.target_endpoint
        ),
        if result.health == "healthy" {
            "info"
        } else {
            "warn"
        },
        format!(
            "link {} -> {} health {}: {}",
            result.source_endpoint, result.target_endpoint, result.health, result.message
        ),
        serde_json::json!({
            "source_endpoint": result.source_endpoint,
            "target_endpoint": result.target_endpoint,
            "health": result.health,
            "latency_ms": result.latency_ms,
            "message": result.message,
        }),
    )
}

fn missing_target_health(link: &Link) -> EndpointHealthResult {
    EndpointHealthResult {
        endpoint: link.target_endpoint.clone(),
        health: "blocked".to_string(),
        reachable: false,
        latency_ms: None,
        message: "target endpoint is missing".to_string(),
    }
}

fn operation_steps(operation: &Operation) -> Vec<serde_json::Value> {
    value_steps(&operation.plan)
}

fn rollback_steps(operation: &Operation) -> Vec<serde_json::Value> {
    value_steps(&operation.rollback_plan)
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
