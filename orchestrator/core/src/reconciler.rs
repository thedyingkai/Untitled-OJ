use crate::{
    DiagnosticReport, EndpointProbe, OperationStatus, OrchestratorStore, Result, TopologySnapshot,
    build_diagnostic_report, build_topology, check_endpoint_health_with_probe, check_link_health,
    expire_operation,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconcileTickResult {
    pub expired_operations: Vec<String>,
    pub checked_endpoints: Vec<String>,
    pub checked_links: Vec<String>,
    pub topology_snapshot_id: Option<String>,
    pub diagnostic_report_id: Option<String>,
}

pub fn run_reconcile_tick<S, P>(
    store: &mut S,
    probe: &P,
    tick_id: impl Into<String>,
) -> Result<ReconcileTickResult>
where
    S: OrchestratorStore,
    P: EndpointProbe,
{
    let tick_id = tick_id.into();
    let mut result = ReconcileTickResult::default();

    for operation in store.list_operations()? {
        if matches!(operation.status, OperationStatus::AwaitingConfirmation)
            && operation.confirmed_at.is_empty()
        {
            let expired = expire_operation(&operation)?;
            store.update_operation(expired.clone())?;
            result.expired_operations.push(expired.operation_id);
        }
    }

    let endpoints = store.list_endpoints()?;
    let mut endpoint_results = BTreeMap::new();
    for endpoint in &endpoints {
        let health = check_endpoint_health_with_probe(endpoint, probe)?;
        store.update_endpoint_health(&health.endpoint, health.health.clone(), health.reachable)?;
        result.checked_endpoints.push(health.endpoint.clone());
        endpoint_results.insert(health.endpoint.clone(), health);
    }

    for link in store.list_links()? {
        let Some(target_health) = endpoint_results.get(&link.target_endpoint) else {
            continue;
        };
        let health = check_link_health(&link, &endpoints, target_health)?;
        store.update_link_health(
            &health.source_endpoint,
            &health.target_endpoint,
            health.health.clone(),
            health.latency_ms,
        )?;
        result.checked_links.push(format!(
            "{} -> {}",
            health.source_endpoint, health.target_endpoint
        ));
    }

    if let Ok(topology) = build_reconciled_topology(store) {
        let snapshot_id = format!("tick-{tick_id}");
        store.save_topology_snapshot(TopologySnapshot {
            snapshot_id: snapshot_id.clone(),
            topology,
            created_at: String::new(),
        })?;
        result.topology_snapshot_id = Some(snapshot_id);
    }

    if store.get_latest_topology_snapshot()?.is_some() {
        let report_id = format!("diag-tick-{tick_id}");
        let report: DiagnosticReport = build_diagnostic_report(store, report_id.clone())?;
        store.create_diagnostic_report(report)?;
        result.diagnostic_report_id = Some(report_id);
    }

    Ok(result)
}

fn build_reconciled_topology<S>(store: &S) -> Result<crate::Topology>
where
    S: OrchestratorStore,
{
    let endpoints = store.list_endpoints()?;
    let root_endpoint = endpoints
        .iter()
        .find(|endpoint| endpoint.service_id == "gateway")
        .or_else(|| endpoints.first())
        .map(|endpoint| endpoint.endpoint.clone())
        .ok_or_else(|| {
            crate::OrchestratorError::Dependency("no endpoint for topology".to_string())
        })?;

    build_topology(
        root_endpoint,
        store
            .list_services()?
            .into_iter()
            .map(|service| service.id)
            .collect(),
        store.list_sets()?.into_iter().map(|set| set.id).collect(),
        endpoints,
        store.list_links()?,
        store.list_operations()?,
        store.list_log_sources()?,
        store.list_diagnostic_reports()?,
    )
}
