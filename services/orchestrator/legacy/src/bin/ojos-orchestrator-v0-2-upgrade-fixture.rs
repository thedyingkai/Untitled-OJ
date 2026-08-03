//! Writes a deterministic v0.2 normalized PostgreSQL fixture through the
//! historical repository implementation. This binary is release-test-only;
//! it is never packaged as a v1 product surface.

#[cfg(not(feature = "legacy-0_2"))]
compile_error!("the v0.2 upgrade fixture must be built with --features legacy-0_2");

use orchestrator_legacy::{
    Endpoint, HostService, Link, OrchestratorStore, PgOrchestratorStore, Topology,
    TopologyAuthority, TopologySnapshot,
};
use serde_json::json;

const CONFIRMATION: &str = "write-v0-2-upgrade-fixture";
const SNAPSHOT_ID: &str = "release-v0-2-upgrade-snapshot";
const WORKER_SERVICE_ID: &str = "release-v0-2-upgrade-worker";

fn main() -> orchestrator_legacy::Result<()> {
    if std::env::var("OJOS_CONFIRM_V0_2_UPGRADE_FIXTURE").as_deref() != Ok(CONFIRMATION) {
        return Err(orchestrator_legacy::OrchestratorError::Blocked(format!(
            "set OJOS_CONFIRM_V0_2_UPGRADE_FIXTURE={CONFIRMATION}"
        )));
    }
    let database_url = std::env::var(PgOrchestratorStore::ENV_NAME).map_err(|_| {
        orchestrator_legacy::OrchestratorError::Dependency(format!(
            "{} is required",
            PgOrchestratorStore::ENV_NAME
        ))
    })?;
    let mut store = PgOrchestratorStore::new(database_url)?;
    let gateway = endpoint("127.0.0.1:18080:release-v0-2-upgrade-gateway", true);
    let worker = endpoint("127.0.0.2:18081:release-v0-2-upgrade-worker", false);
    let snapshot = TopologySnapshot {
        snapshot_id: SNAPSHOT_ID.to_string(),
        topology: Topology {
            root_host: "127.0.0.1".to_string(),
            root_endpoint: gateway.endpoint.clone(),
            authority: TopologyAuthority {
                root_host: "127.0.0.1".to_string(),
                root_endpoint: gateway.endpoint.clone(),
                exposure_policy: "private".to_string(),
                notes: vec!["v0.2 release upgrade fixture".to_string()],
            },
            services: vec![gateway.service_id.clone(), worker.service_id.clone()],
            endpoints: vec![gateway.clone(), worker.clone()],
            links: vec![Link {
                source_endpoint: gateway.endpoint.clone(),
                target_endpoint: worker.endpoint.clone(),
                protocol: "http".to_string(),
                auth_mode: "service".to_string(),
                scope: "internal".to_string(),
                enabled: true,
                health: "healthy".to_string(),
                latency_ms: Some(4),
                config_ref: String::new(),
                secret_ref: String::new(),
                policy: json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            }],
            operations: Vec::new(),
            log_views: Vec::new(),
            diagnostic_reports: Vec::new(),
        },
        created_at: "2026-08-03T00:00:00Z".to_string(),
    };

    store.upsert_host_service(HostService {
        host_ip: "127.0.0.2".to_string(),
        service_name: WORKER_SERVICE_ID.to_string(),
        version: "0.2.0".to_string(),
        status: "running".to_string(),
        config: json!({"written_by": "v0.2-repository"}),
        labels: json!({"upgrade_fixture": true}),
        created_at: String::new(),
        updated_at: String::new(),
    })?;
    store.upsert_endpoint(gateway)?;
    store.upsert_endpoint(worker)?;
    store.save_topology_snapshot(snapshot)?;

    let persisted = store.get_latest_topology_snapshot()?.ok_or_else(|| {
        orchestrator_legacy::OrchestratorError::Dependency(
            "v0.2 upgrade topology snapshot".to_string(),
        )
    })?;
    if persisted.snapshot_id != SNAPSHOT_ID {
        return Err(orchestrator_legacy::OrchestratorError::Dependency(
            "v0.2 repository did not read back the written snapshot".to_string(),
        ));
    }
    println!(
        "{}",
        json!({
            "schema": "orchestrator-v0.2-normalized",
            "snapshot_id": SNAPSHOT_ID,
            "service_id": WORKER_SERVICE_ID,
            "version": "0.2.0"
        })
    );
    Ok(())
}

fn endpoint(identity: &str, reachable: bool) -> Endpoint {
    let service_id = identity.rsplit(':').next().unwrap_or_default().to_string();
    Endpoint {
        endpoint: identity.to_string(),
        service_id: service_id.clone(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: if reachable { "healthy" } else { "unreachable" }.to_string(),
        reachable,
        display_name: service_id,
        note: String::new(),
        config: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }
}
