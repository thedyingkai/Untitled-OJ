use orchestrator_core::{
    Endpoint, Link, MemoryOrchestratorStore, OperationExecutor, OperationStatus, OrchestratorStore,
    build_topology, confirm_operation, ensure_shared_schemas_loaded, load_shared_schemas,
    service_install_operation, validate_service_manifest_file,
};
use serde_json::json;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn shared_schema_registry_is_public_core_contract() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");

    ensure_shared_schemas_loaded(&schemas).expect("shared schemas should be usable");
    assert!(schemas.actions.contains(&"service.install".to_string()));
    assert!(schemas.actions.contains(&"topology.apply".to_string()));
    assert_eq!(schemas.action_count(), schemas.form_count());
}

#[test]
fn service_install_operation_requires_confirmation_before_apply() {
    let root = repo_root();
    let manifest =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml"))
            .expect("gateway service manifest should validate");
    let planned = service_install_operation("op-install-gateway", &manifest, &[])
        .expect("service install plan should be created");

    assert_eq!(planned.status, OperationStatus::Planned);
    assert_eq!(planned.action, "service.install");
    assert_eq!(planned.target_type, "Service");
    assert_eq!(
        planned.plan["steps"][0]["detail"],
        json!("写入 services 表")
    );

    let confirmed = confirm_operation(&planned).expect("operation should confirm");
    let mut store = MemoryOrchestratorStore::new();
    store
        .put_operation(planned)
        .expect("planned operation should store");

    let direct_apply = OperationExecutor::new(&mut store).apply("op-install-gateway");
    assert!(
        direct_apply.is_err(),
        "planned operation must not apply without confirmation"
    );

    store
        .put_operation(confirmed)
        .expect("confirmed operation should store");
    let applied = OperationExecutor::new(&mut store)
        .apply("op-install-gateway")
        .expect("confirmed operation should apply");

    assert_eq!(applied.status, OperationStatus::Succeeded);
    assert_eq!(store.operation_logs("op-install-gateway").len(), 2);
}

#[test]
fn topology_contract_uses_endpoint_and_link_identity() {
    let endpoints = vec![
        Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
        Endpoint {
            endpoint: "127.0.0.1:8081".to_string(),
            service_id: "problem-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let links = vec![Link {
        source_endpoint: "127.0.0.1:8080".to_string(),
        target_endpoint: "127.0.0.1:8081".to_string(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "root-managed".to_string(),
        health: "ok".to_string(),
        latency_ms: Some(1),
        config_ref: String::new(),
        secret_ref: "secret_ref:gateway-problem-api".to_string(),
        policy: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }];

    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints,
        links,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("topology should build from endpoint and link identities");

    assert_eq!(topology.root_host, "127.0.0.1");
    assert_eq!(topology.links[0].source_endpoint, "127.0.0.1:8080");
    assert_eq!(topology.links[0].target_endpoint, "127.0.0.1:8081");
    assert_eq!(topology.authority.exposure_policy, "root-host-gui-tui-only");
}
