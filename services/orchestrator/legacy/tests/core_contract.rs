use orchestrator_legacy::{
    Endpoint, Link, MemoryOrchestratorStore, OperationExecutor, OperationStatus, OrchestratorStore,
    build_topology, confirm_operation, ensure_shared_schemas_loaded, load_shared_schemas,
    release_install_operation, validate_service_manifest_file,
};
use serde_json::json;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    let mut current = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if current
            .join("platform/schemas/orchestrator/actions.yaml")
            .is_file()
            && current
                .join("services/orchestrator/core/Cargo.toml")
                .is_file()
        {
            return current;
        }
        if !current.pop() {
            panic!("repo root");
        }
    }
}

#[test]
fn shared_schema_registry_is_public_core_contract() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");

    ensure_shared_schemas_loaded(&schemas).expect("shared schemas should be usable");
    assert!(schemas.actions.contains(&"release.install".to_string()));
    assert!(schemas.actions.contains(&"topology.apply".to_string()));
    assert_eq!(schemas.action_count(), schemas.form_count());
}

#[test]
fn release_install_operation_requires_confirmation_before_apply() {
    let root = repo_root();
    let manifest =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml"))
            .expect("gateway service manifest should validate");
    let planned = release_install_operation("op-install-gateway", &manifest, &[])
        .expect("release install plan should be created");

    assert_eq!(planned.status, OperationStatus::Planned);
    assert_eq!(planned.action, "release.install");
    assert_eq!(planned.target_type, "ServiceRelease");
    let step_actions = planned
        .plan
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .expect("release.install steps")
        .iter()
        .filter_map(|step| step.get("action").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    assert!(step_actions.contains(&"validate_service_manifest"));
    assert!(step_actions.contains(&"create_host_service"));
    assert!(step_actions.contains(&"allocate_endpoint"));
    assert!(step_actions.contains(&"health_probe"));
    assert_eq!(planned.request["endpoint"], json!("127.0.0.1:8080:gateway"));

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
    assert!(
        store
            .operation_logs("op-install-gateway")
            .iter()
            .any(|record| !record.step_id.is_empty()),
        "confirmed apply should persist step logs"
    );
}

#[test]
fn topology_contract_uses_endpoint_and_link_identity() {
    let endpoints = vec![
        Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
            endpoint: "127.0.0.1:8081:problem-service".to_string(),
            service_id: "problem-service".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Problem Service".to_string(),
            note: String::new(),
            config: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let links = vec![Link {
        source_endpoint: "127.0.0.1:8080:gateway".to_string(),
        target_endpoint: "127.0.0.1:8081:problem-service".to_string(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "root-managed".to_string(),
        enabled: true,
        health: "ok".to_string(),
        latency_ms: Some(1),
        config_ref: String::new(),
        secret_ref: "secret_ref:gateway-problem-service".to_string(),
        policy: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }];

    let topology = build_topology(
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
        endpoints,
        links,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("topology should build from endpoint and link identities");

    assert_eq!(topology.root_host, "127.0.0.1");
    assert_eq!(topology.links[0].source_endpoint, "127.0.0.1:8080:gateway");
    assert_eq!(
        topology.links[0].target_endpoint,
        "127.0.0.1:8081:problem-service"
    );
    assert_eq!(topology.authority.exposure_policy, "root-host-web-tui-only");
}
