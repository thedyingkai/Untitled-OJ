use orchestrator_core::{
    Endpoint, EndpointDecl, Link, LogView, OperationLogRecord, OperationStatus, OrchestratorStore,
    PgOrchestratorStore, RuntimeMode, ServiceHealthDecl, ServiceManifest, ServiceProvides,
    ServiceRequires, ServiceRuntimeDecl, ServiceSecurityDecl, SourceDecl, TopologySnapshot,
    build_diagnostic_report, build_topology, parse_endpoint_id, plan_operation,
};
use postgres::{Client, NoTls};
use serde_json::json;

#[test]
#[ignore = "requires a migrated PostgreSQL database and ORCHESTRATOR_DATABASE_URL"]
fn pg_store_persists_core_objects_and_reads_them_back() {
    let suffix = unique_suffix();
    let database_url = std::env::var(PgOrchestratorStore::ENV_NAME)
        .expect("ORCHESTRATOR_DATABASE_URL should be set");
    let mut store =
        PgOrchestratorStore::from_env().expect("ORCHESTRATOR_DATABASE_URL should be set");

    let gateway = service_manifest(format!("gateway-{suffix}"), 18080);
    let problem_service = service_manifest(format!("problem-service-{suffix}"), 18083);
    store
        .upsert_service(gateway.clone())
        .expect("upsert gateway service");
    store
        .upsert_service(problem_service.clone())
        .expect("upsert problem-service service");

    let gateway_port = 18080 + port_offset(&suffix);
    let problem_port = 18083 + port_offset(&suffix);
    let gateway_endpoint = Endpoint {
        endpoint: format!("127.0.0.1:{gateway_port}:{}", gateway.id),
        service_id: gateway.id.clone(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Gateway".to_string(),
        note: "integration test".to_string(),
        config: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let problem_endpoint = Endpoint {
        endpoint: format!("127.0.0.1:{problem_port}:{}", problem_service.id),
        service_id: problem_service.id.clone(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Problem Service".to_string(),
        note: "integration test".to_string(),
        config: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    store
        .upsert_endpoint(gateway_endpoint.clone())
        .expect("upsert gateway endpoint");
    store
        .upsert_endpoint(problem_endpoint.clone())
        .expect("upsert problem endpoint");
    store
        .update_endpoint_health(&gateway_endpoint.endpoint, "healthy".to_string(), true)
        .expect("update endpoint health");

    let link = Link {
        source_endpoint: gateway_endpoint.endpoint.clone(),
        target_endpoint: problem_endpoint.endpoint.clone(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "api".to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    store.upsert_link(link.clone()).expect("upsert link");
    store
        .update_link_health(
            &link.source_endpoint,
            &link.target_endpoint,
            "degraded".to_string(),
            Some(7),
        )
        .expect("update link health");

    let mut operation = plan_operation(
        format!("op-{suffix}"),
        "endpoint.health.check",
        "Endpoint",
        &gateway_endpoint.endpoint,
        json!({"endpoint": gateway_endpoint.endpoint}),
        json!({"steps": [{"id": "check", "action": "endpoint.health.check"}]}),
        json!({}),
    )
    .expect("plan operation");
    operation.status = OperationStatus::Running;
    store
        .create_operation(operation.clone())
        .expect("create operation");
    store
        .append_operation_log(OperationLogRecord {
            operation_id: operation.operation_id.clone(),
            step_id: "check".to_string(),
            level: "info".to_string(),
            message: "health checked".to_string(),
            data: json!({"endpoint": gateway_endpoint.endpoint}),
            redacted: false,
            created_at: String::new(),
        })
        .expect("append operation log");
    store
        .update_operation_status(
            &operation.operation_id,
            OperationStatus::Succeeded,
            String::new(),
        )
        .expect("update operation status");
    store
        .update_operation_result(&operation.operation_id, json!({"status": "ok"}))
        .expect("update operation result");

    let log_view = LogView {
        source_id: format!("log-{suffix}"),
        service_id: gateway.id.clone(),
        endpoint: gateway_endpoint.endpoint.clone(),
        operation_id: operation.operation_id.clone(),
        path: "/logs".to_string(),
        driver: "external-endpoint".to_string(),
        read_policy: "service-scoped".to_string(),
        display_name: "Gateway logs".to_string(),
    };
    store
        .upsert_log_source(log_view.clone())
        .expect("upsert log source");

    let topology = build_topology(
        gateway_endpoint.endpoint.clone(),
        vec![gateway.id.clone(), problem_service.id.clone()],
        vec![
            store
                .get_endpoint(&gateway_endpoint.endpoint)
                .expect("read gateway endpoint")
                .expect("gateway endpoint exists"),
            store
                .get_endpoint(&problem_endpoint.endpoint)
                .expect("read problem endpoint")
                .expect("problem endpoint exists"),
        ],
        vec![
            store
                .get_link(&link.source_endpoint, &link.target_endpoint)
                .expect("read link")
                .expect("link exists"),
        ],
        store.list_operations().expect("list operations"),
        vec![log_view.clone()],
        Vec::new(),
    )
    .expect("build topology");
    store
        .save_topology_snapshot(TopologySnapshot {
            snapshot_id: format!("snapshot-{suffix}"),
            topology,
            created_at: String::new(),
        })
        .expect("save topology snapshot");

    let report =
        build_diagnostic_report(&store, format!("diag-{suffix}")).expect("build diagnostic report");
    store
        .create_diagnostic_report(report.clone())
        .expect("create diagnostic report");

    assert_eq!(
        store
            .get_service(&gateway.id)
            .expect("get service")
            .expect("service exists")
            .id,
        gateway.id
    );
    let gateway_endpoints = store
        .list_endpoints()
        .expect("list endpoints")
        .into_iter()
        .filter(|endpoint| endpoint.service_id == gateway.id)
        .collect::<Vec<_>>();
    assert_eq!(gateway_endpoints.len(), 1);
    assert_eq!(
        parse_endpoint_id(&gateway_endpoint.endpoint)
            .expect("gateway endpoint identity")
            .service_name,
        gateway.id
    );
    assert_eq!(
        store
            .get_endpoint(&gateway_endpoint.endpoint)
            .expect("get endpoint")
            .expect("endpoint exists")
            .health,
        "healthy"
    );
    assert_eq!(
        store
            .get_link(&link.source_endpoint, &link.target_endpoint)
            .expect("get link")
            .expect("link exists")
            .latency_ms,
        Some(7)
    );
    assert_eq!(
        store
            .get_operation(&operation.operation_id)
            .expect("get operation")
            .expect("operation exists")
            .status,
        OperationStatus::Succeeded
    );
    assert_eq!(
        store
            .list_operation_logs(&operation.operation_id)
            .expect("list operation logs")
            .len(),
        1
    );
    assert!(
        store
            .get_latest_topology_snapshot()
            .expect("latest topology")
            .is_some()
    );
    assert_eq!(
        store
            .get_diagnostic_report(&report.report_id)
            .expect("get diagnostic")
            .expect("diagnostic exists")
            .report_id,
        report.report_id
    );

    cleanup(
        &database_url,
        &mut store,
        &gateway.id,
        &problem_service.id,
        &operation.operation_id,
        &log_view.source_id,
        &report.report_id,
        &format!("snapshot-{suffix}"),
    );
}

fn service_manifest(id: String, port: u16) -> ServiceManifest {
    ServiceManifest {
        schema_version: 1,
        name: format!("{id} integration"),
        id,
        version: "0.1.0".to_string(),
        kind: "backend-api".to_string(),
        description: "Pg store integration service".to_string(),
        endpoint: EndpointDecl {
            protocol: "http".to_string(),
            default_port: port,
            health_path: "/health".to_string(),
            expose: true,
            routes: Vec::new(),
        },
        runtime: ServiceRuntimeDecl {
            mode: RuntimeMode::External,
            driver: "external-endpoint".to_string(),
            root_allowed: true,
            non_root_allowed: false,
            start_policy: "manual".to_string(),
            restart_policy: "manual".to_string(),
        },
        config_schema: json!({}),
        requires: ServiceRequires::default(),
        provides: ServiceProvides::default(),
        ui: Default::default(),
        permissions: Vec::new(),
        security: ServiceSecurityDecl::default(),
        source: SourceDecl {
            r#type: "local".to_string(),
            reference: "services/test".to_string(),
            build: json!({}),
            artifact: json!({}),
        },
        health: ServiceHealthDecl {
            checks: vec!["http".to_string()],
            timeout_seconds: 3,
            interval_seconds: 10,
        },
        resources: json!({}),
    }
}

fn unique_suffix() -> String {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    format!("it-{value}")
}

fn port_offset(value: &str) -> u16 {
    value
        .bytes()
        .fold(0_u16, |acc, item| acc.wrapping_add(u16::from(item)))
        % 1000
}

fn cleanup(
    database_url: &str,
    store: &mut PgOrchestratorStore,
    gateway_id: &str,
    problem_id: &str,
    operation_id: &str,
    log_source_id: &str,
    report_id: &str,
    snapshot_id: &str,
) {
    let _ = store.delete_log_source(log_source_id);
    let _ = store.delete_service(gateway_id);
    let _ = store.delete_service(problem_id);
    let _ = store.release_operation_lock(&format!("operation:{operation_id}"), operation_id);
    if let Ok(mut client) = Client::connect(database_url, NoTls) {
        let _ = client.execute(
            "DELETE FROM orchestrator_operation_logs WHERE operation_id = $1",
            &[&operation_id],
        );
        let _ = client.execute(
            "DELETE FROM orchestrator_operation_locks WHERE operation_id = $1",
            &[&operation_id],
        );
        let _ = client.execute(
            "DELETE FROM orchestrator_operations WHERE operation_id = $1",
            &[&operation_id],
        );
        let _ = client.execute(
            "DELETE FROM topology_snapshots WHERE snapshot_id = $1",
            &[&snapshot_id],
        );
        let _ = client.execute(
            "DELETE FROM diagnostic_reports WHERE report_id = $1",
            &[&report_id],
        );
    }
}
