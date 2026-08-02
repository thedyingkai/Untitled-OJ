use orchestrator_core::{
    DeferredAuthPermissionRegistrar, DeferredRedisResourceProvisioner,
    DeferredReleasePackageLoader, DeferredStorageResourceProvisioner, Endpoint, EndpointDecl,
    EndpointHealthResult, EndpointProbe, Link, LocalSqlMigrationRunner, LogView, OperationExecutor,
    OperationLogRecord, OperationStatus, OrchestratorStore, PgOrchestratorStore,
    ReleaseBackendDecl, ReleaseFrontendDecl, ReleaseMigrationDecl, ReleaseObservabilityDecl,
    ReleaseRuntimeDecl, ReleaseServiceIdentityDecl, ReleaseSourceDecl, RuntimeMode,
    ServiceHealthDecl, ServiceManifest, ServiceProvides, ServiceReleaseManifest, ServiceRequires,
    ServiceRuntimeDecl, ServiceSecurityDecl, SourceDecl, TopologySnapshot, build_diagnostic_report,
    build_topology, confirm_operation, parse_endpoint_id, plan_operation,
    release_install_operation_with_release,
};
use postgres::{Client, NoTls};
use serde_json::json;
use std::fs;

#[test]
fn pg_store_persists_core_objects_and_reads_them_back() {
    let suffix = unique_suffix();
    let Some(database_url) = pg_live_database_url() else {
        return;
    };
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
        enabled: true,
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

#[test]
fn migration_live_postgres_release_install_pipeline_records_real_statuses() {
    let suffix = unique_suffix();
    let Some(database_url) = pg_live_database_url() else {
        return;
    };
    let temp = tempfile::tempdir().expect("temp migration root");
    let mut store =
        PgOrchestratorStore::from_env().expect("ORCHESTRATOR_DATABASE_URL should be set");

    let service_name = format!("migration-live-{suffix}");
    let table = sql_identifier(&format!("{service_name}-objects"));
    let service = service_manifest(service_name.clone(), 19080 + port_offset(&suffix));
    let migration_sql = format!(
        "CREATE TABLE IF NOT EXISTS {table} (id INT PRIMARY KEY);\nINSERT INTO {table} (id) VALUES (1) ON CONFLICT DO NOTHING;\n"
    );
    let migration = write_live_migration(temp.path(), &service_name, "0001", &migration_sql, false);
    let release = release_for_service(&service, vec![migration]);

    apply_live_release_install(
        &mut store,
        &format!("op-{service_name}-first"),
        &service,
        &release,
        temp.path(),
        &database_url,
        false,
    )
    .expect("first live migration install");
    assert_eq!(
        table_row_count(&database_url, &table),
        1,
        "migration SQL should apply to real Postgres"
    );
    let records = records_for_service(&store, &service_name);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "applied");

    apply_live_release_install(
        &mut store,
        &format!("op-{service_name}-second"),
        &service,
        &release,
        temp.path(),
        &database_url,
        false,
    )
    .expect("repeat install should skip applied migration");
    assert_eq!(
        table_row_count(&database_url, &table),
        1,
        "repeat install must not re-run already-applied migration"
    );
    let logs = store
        .list_operation_logs(&format!("op-{service_name}-second"))
        .expect("second operation logs");
    assert!(logs.iter().any(|log| {
        log.step_id == format!("migrations:{service_name}")
            && log.data.get("runner").and_then(serde_json::Value::as_str) == Some("already-applied")
    }));

    let mut mismatched = release.clone();
    mismatched.migrations[0].checksum = "len:1".to_string();
    let err = apply_live_release_install(
        &mut store,
        &format!("op-{service_name}-checksum-mismatch"),
        &service,
        &mismatched,
        temp.path(),
        &database_url,
        false,
    )
    .expect_err("checksum mismatch should fail");
    assert!(
        err.to_string().contains("already applied with checksum"),
        "unexpected checksum mismatch error: {err}"
    );

    let destructive_service_name = format!("migration-live-destructive-{suffix}");
    let destructive_table = sql_identifier(&format!("{destructive_service_name}-objects"));
    let destructive_service = service_manifest(
        destructive_service_name.clone(),
        19180 + port_offset(&suffix),
    );
    let destructive_sql = format!("CREATE TABLE IF NOT EXISTS {destructive_table} (id INT);\n");
    let destructive_migration = write_live_migration(
        temp.path(),
        &destructive_service_name,
        "0001",
        &destructive_sql,
        true,
    );
    let destructive_release =
        release_for_service(&destructive_service, vec![destructive_migration]);
    let err = apply_live_release_install(
        &mut store,
        &format!("op-{destructive_service_name}-blocked"),
        &destructive_service,
        &destructive_release,
        temp.path(),
        &database_url,
        false,
    )
    .expect_err("destructive migration should require explicit allowance");
    assert!(
        err.to_string().contains("destructive migration"),
        "unexpected destructive migration error: {err}"
    );
    assert_eq!(
        records_for_service(&store, &destructive_service_name)[0].status,
        "failed"
    );
    apply_live_release_install(
        &mut store,
        &format!("op-{destructive_service_name}-allowed"),
        &destructive_service,
        &destructive_release,
        temp.path(),
        &database_url,
        true,
    )
    .expect("explicitly allowed destructive migration should apply");
    assert_eq!(
        records_for_service(&store, &destructive_service_name)[0].status,
        "applied"
    );

    let failed_service_name = format!("migration-live-failed-{suffix}");
    let failed_service =
        service_manifest(failed_service_name.clone(), 19280 + port_offset(&suffix));
    let failed_migration = write_live_migration(
        temp.path(),
        &failed_service_name,
        "0001",
        "THIS IS NOT SQL;\n",
        false,
    );
    let failed_release = release_for_service(&failed_service, vec![failed_migration]);
    apply_live_release_install(
        &mut store,
        &format!("op-{failed_service_name}"),
        &failed_service,
        &failed_release,
        temp.path(),
        &database_url,
        false,
    )
    .expect_err("invalid SQL should fail");
    assert_eq!(
        records_for_service(&store, &failed_service_name)[0].status,
        "failed"
    );

    cleanup_live_migration(
        &database_url,
        &[
            &service_name,
            &destructive_service_name,
            &failed_service_name,
        ],
        &[&table, &destructive_table],
        &format!("op-migration-live-{suffix}"),
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

fn release_for_service(
    service: &ServiceManifest,
    migrations: Vec<ReleaseMigrationDecl>,
) -> ServiceReleaseManifest {
    ServiceReleaseManifest {
        schema_version: 1,
        service_name: service.id.clone(),
        version: service.version.clone(),
        description: "Live migration integration release".to_string(),
        service_type: service.kind.clone(),
        source: ReleaseSourceDecl {
            kind: "local".to_string(),
            url: format!("local://services/{}", service.id),
            checksum: String::new(),
        },
        runtime: ReleaseRuntimeDecl {
            kind: "local-process".to_string(),
            image: String::new(),
            binary: String::new(),
            system_service: String::new(),
            command: "sleep".to_string(),
            args: vec!["1".to_string()],
            working_dir: String::new(),
            env: Default::default(),
        },
        frontend: ReleaseFrontendDecl::default(),
        backend: ReleaseBackendDecl {
            protocol: service.endpoint.protocol.clone(),
            port: service.endpoint.default_port,
            health_path: service.endpoint.health_path.clone(),
        },
        migrations,
        permissions: Vec::new(),
        routes: Vec::new(),
        apis: Vec::new(),
        redis: Vec::new(),
        storage: Vec::new(),
        dependencies: Vec::new(),
        required_apis: Vec::new(),
        service_identity: ReleaseServiceIdentityDecl::default(),
        config_schema: json!({}),
        secrets: Vec::new(),
        observability: ReleaseObservabilityDecl::default(),
    }
}

fn write_live_migration(
    root: &std::path::Path,
    service_name: &str,
    version: &str,
    sql: &str,
    destructive: bool,
) -> ReleaseMigrationDecl {
    let relative = format!("services/{service_name}/migrations/{version}.sql");
    let path = root.join(&relative);
    fs::create_dir_all(path.parent().expect("migration parent")).expect("migration dir");
    fs::write(&path, sql).expect("write migration sql");
    ReleaseMigrationDecl {
        version: version.to_string(),
        path: relative,
        checksum: format!("len:{}", sql.len()),
        destructive,
    }
}

fn apply_live_release_install(
    store: &mut PgOrchestratorStore,
    operation_id: &str,
    service: &ServiceManifest,
    release: &ServiceReleaseManifest,
    migration_root: &std::path::Path,
    database_url: &str,
    allow_destructive: bool,
) -> orchestrator_core::Result<orchestrator_core::Operation> {
    let operation = release_install_operation_with_release(
        operation_id,
        service,
        Some(release),
        &[],
        "127.0.0.1",
        None,
        json!({
            "external_service_running": true,
            "allow_destructive_migrations": allow_destructive
        }),
    )
    .and_then(|operation| confirm_operation(&operation))?;
    store.create_operation(operation)?;
    OperationExecutor::with_runtime_provisioners_and_release_loader(
        store,
        HealthyMigrationEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        LocalSqlMigrationRunner::new(migration_root).with_database_url(database_url),
        DeferredReleasePackageLoader,
    )
    .apply(operation_id)
}

#[derive(Debug, Default, Clone)]
struct HealthyMigrationEndpointProbe;

impl EndpointProbe for HealthyMigrationEndpointProbe {
    fn probe(&self, endpoint: &Endpoint) -> orchestrator_core::Result<EndpointHealthResult> {
        Ok(EndpointHealthResult {
            endpoint: endpoint.endpoint.clone(),
            health: "healthy".to_string(),
            reachable: true,
            latency_ms: Some(0),
            message: "migration integration fixture reports the external service healthy"
                .to_string(),
        })
    }
}

fn records_for_service(
    store: &PgOrchestratorStore,
    service_name: &str,
) -> Vec<orchestrator_core::ServiceMigrationRecord> {
    store
        .list_service_migration_records()
        .expect("list migration records")
        .into_iter()
        .filter(|record| record.service_name == service_name)
        .collect()
}

fn table_row_count(database_url: &str, table: &str) -> i64 {
    let mut client = Client::connect(database_url, NoTls).expect("connect live migration db");
    let query = format!("SELECT COUNT(*) FROM {table}");
    client
        .query_one(&query, &[])
        .expect("count migrated rows")
        .get(0)
}

fn sql_identifier(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
}

fn cleanup_live_migration(
    database_url: &str,
    service_names: &[&str],
    tables: &[&str],
    _operation_prefix: &str,
) {
    let Ok(mut client) = Client::connect(database_url, NoTls) else {
        return;
    };
    for table in tables {
        let _ = client.batch_execute(&format!("DROP TABLE IF EXISTS {table}"));
    }
    for service_name in service_names {
        let operation_prefix = format!("op-{service_name}%");
        let operation_ids = client
            .query(
                "SELECT operation_id FROM orchestrator_operations WHERE operation_id LIKE $1 OR target_id = $2",
                &[&operation_prefix, service_name],
            )
            .map(|rows| {
                rows.into_iter()
                    .map(|row| row.get::<_, String>(0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for operation_id in operation_ids {
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
        }
        let _ = client.execute(
            "DELETE FROM service_migration_records WHERE service_name = $1",
            &[service_name],
        );
        let _ = client.execute(
            "DELETE FROM host_services WHERE service_name = $1",
            &[service_name],
        );
        let _ = client.execute(
            "DELETE FROM services WHERE service_id = $1",
            &[service_name],
        );
        let endpoint_like = format!("%:{service_name}");
        let _ = client.execute(
            "DELETE FROM service_endpoints WHERE endpoint LIKE $1",
            &[&endpoint_like],
        );
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

fn pg_live_database_url() -> Option<String> {
    match std::env::var(PgOrchestratorStore::ENV_NAME)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(value),
        None if require_pg_live() => {
            panic!(
                "{} must be set when OJOS_REQUIRE_PG_LIVE=1",
                PgOrchestratorStore::ENV_NAME
            )
        }
        None => {
            eprintln!(
                "skipping PostgreSQL live integration: {} is not set; CI sets OJOS_REQUIRE_PG_LIVE=1 so this cannot silently pass in production gates",
                PgOrchestratorStore::ENV_NAME
            );
            None
        }
    }
}

fn require_pg_live() -> bool {
    std::env::var("OJOS_REQUIRE_PG_LIVE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

// Cleanup keeps each independently named fixture explicit at the call site.
#[allow(clippy::too_many_arguments)]
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
