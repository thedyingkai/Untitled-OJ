use crate::*;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

fn repo_root() -> PathBuf {
    let mut current = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if current.join("Cargo.toml").is_file()
            && current.join("services").is_dir()
            && current.join("sets").is_dir()
        {
            return current;
        }
        if !current.pop() {
            panic!("repo root");
        }
    }
}

fn valid_service() -> ServiceManifest {
    ServiceManifest {
        schema_version: 1,
        id: "demo-api".to_string(),
        name: "Demo API".to_string(),
        version: "0.1.0".to_string(),
        kind: "backend-api".to_string(),
        description: "Demo service".to_string(),
        endpoint: EndpointDecl {
            protocol: "http".to_string(),
            default_port: 18080,
            health_path: "/health".to_string(),
            expose: true,
            routes: vec!["/demo".to_string()],
        },
        runtime: ServiceRuntimeDecl {
            mode: RuntimeMode::Container,
            driver: "container".to_string(),
            root_allowed: true,
            non_root_allowed: false,
            start_policy: "manual".to_string(),
            restart_policy: "on-failure".to_string(),
        },
        config_schema: serde_json::json!({}),
        requires: Default::default(),
        provides: Default::default(),
        ui: Default::default(),
        permissions: vec!["demo.read".to_string()],
        security: Default::default(),
        source: SourceDecl {
            r#type: "local".to_string(),
            reference: "services/demo-api".to_string(),
            build: serde_json::json!({}),
            artifact: serde_json::json!({}),
        },
        health: ServiceHealthDecl {
            checks: vec!["http".to_string()],
            timeout_seconds: 3,
            interval_seconds: 10,
        },
        resources: serde_json::json!({}),
    }
}

fn action_set(root: &Path) -> HashSet<String> {
    let text = fs::read_to_string(root.join("orchestrator/schemas/actions.yaml")).unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    value
        .get("actions")
        .and_then(serde_yaml::Value::as_sequence)
        .expect("actions should be a sequence")
        .iter()
        .map(|item| {
            item.as_str()
                .expect("action should be a string")
                .to_string()
        })
        .collect::<HashSet<_>>()
}

fn relative_files(root: &Path, dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_relative_files(root, dir, &mut files);
    files.sort();
    files
}

fn collect_relative_files(root: &Path, dir: &Path, files: &mut Vec<String>) {
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).expect("read directory") {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_relative_files(root, &path, files);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("file should stay below root")
                .to_string_lossy()
                .replace('\\', "/");
            files.push(rel);
        }
    }
}

#[test]
fn checked_in_service_manifests_validate() {
    let root = repo_root();
    let expected = [
        "services/gateway/service.yaml",
        "services/web-shell/service.yaml",
        "services/problem-api/service.yaml",
        "services/judge-api/service.yaml",
        "services/judge-worker/service.yaml",
        "services/auth/service.yaml",
        "services/redis/service.yaml",
        "services/storage/service.yaml",
        "services/postgres/service.yaml",
    ];
    let mut actual = relative_files(&root, &root.join("services"))
        .into_iter()
        .filter(|path| path.ends_with("/service.yaml"))
        .collect::<Vec<_>>();
    actual.sort();
    assert_eq!(
        actual,
        {
            let mut items = expected
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>();
            items.sort();
            items
        },
        "checked-in Service manifests should stay at the formal service.yaml set"
    );

    for path in expected {
        validate_service_manifest_file(&root, Path::new(path))
            .unwrap_or_else(|err| panic!("{path} should validate: {err}"));
    }
}

#[test]
fn sdk_service_template_matches_formal_manifest_contract() {
    let root = repo_root();
    let text = fs::read_to_string(root.join("sdk/templates/service.yaml"))
        .expect("SDK service template should exist");
    let manifest: ServiceManifest =
        serde_yaml::from_str(&text).expect("SDK service template should parse");

    validate_service_manifest(&manifest).expect("SDK service template should validate");
    assert_eq!(manifest.id, "example-service");
    assert_eq!(manifest.kind, "backend-api");
    assert_eq!(manifest.source.r#type, "local");
    assert!(
        !manifest.health.checks.is_empty(),
        "SDK service template should include health checks"
    );
}

#[test]
fn service_schema_rejects_dangerous_fields() {
    for field in [
        "image",
        "host_path",
        "privileged",
        "cap_add",
        "command",
        "script",
    ] {
        let text = format!(
            r#"
schema_version: 1
id: demo-api
name: Demo API
version: 0.1.0
kind: backend-api
endpoint:
  protocol: http
  default_port: 18080
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
{field}: dangerous
"#
        );
        assert!(
            serde_yaml::from_str::<ServiceManifest>(&text).is_err(),
            "{field} should be rejected by deny_unknown_fields"
        );
    }
}

#[test]
fn service_security_flags_are_rejected() {
    let mut manifest = valid_service();
    manifest.security.allow_privileged = true;
    assert!(validate_service_manifest(&manifest).is_err());

    let mut manifest = valid_service();
    manifest.security.allow_host_mount = true;
    assert!(validate_service_manifest(&manifest).is_err());

    let mut manifest = valid_service();
    manifest.security.allow_arbitrary_command = true;
    assert!(validate_service_manifest(&manifest).is_err());
}

#[test]
fn service_manifest_requires_formal_contract_sections() {
    for missing in ["requires", "provides", "source", "health"] {
        let text = format!(
            r#"
schema_version: 1
id: demo-api
name: Demo API
version: 0.1.0
kind: backend-api
endpoint:
  protocol: http
  default_port: 18080
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
{requires}
{provides}
source:
  type: local
  ref: services/demo-api
health:
  checks: [http]
  timeout_seconds: 3
  interval_seconds: 10
"#,
            requires = if missing == "requires" {
                ""
            } else {
                "requires: {}\n"
            },
            provides = if missing == "provides" {
                ""
            } else {
                "provides:\n  capabilities: [demo.read]\n"
            },
        );
        let text = if missing == "source" {
            text.replace("source:\n  type: local\n  ref: services/demo-api\n", "")
        } else if missing == "health" {
            text.replace(
                "health:\n  checks: [http]\n  timeout_seconds: 3\n  interval_seconds: 10\n",
                "",
            )
        } else {
            text
        };
        assert!(
            serde_yaml::from_str::<ServiceManifest>(&text).is_err(),
            "{missing} section should be required"
        );
    }
}

#[test]
fn service_manifest_requires_source_and_health_values() {
    let mut manifest = valid_service();
    manifest.source.r#type.clear();
    assert!(validate_service_manifest(&manifest).is_err());

    let mut manifest = valid_service();
    manifest.source.reference.clear();
    assert!(validate_service_manifest(&manifest).is_err());

    let mut manifest = valid_service();
    manifest.health.checks.clear();
    assert!(validate_service_manifest(&manifest).is_err());

    let mut manifest = valid_service();
    manifest.health.timeout_seconds = 0;
    assert!(validate_service_manifest(&manifest).is_err());

    let mut manifest = valid_service();
    manifest.health.interval_seconds = 0;
    assert!(validate_service_manifest(&manifest).is_err());
}

#[test]
fn service_manifest_path_stays_under_services() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("services/demo")).unwrap();
    fs::write(
        dir.path().join("services/demo/service.yaml"),
        serde_yaml::to_string(&valid_service()).unwrap(),
    )
    .unwrap();
    assert!(
        validate_service_manifest_file(dir.path(), Path::new("services/demo/service.yaml")).is_ok()
    );
    assert!(validate_service_manifest_file(dir.path(), Path::new("../service.yaml")).is_err());
    assert!(
        validate_service_manifest_file(dir.path(), Path::new("services/.tmp/service.yaml"))
            .is_err()
    );
}

#[test]
fn endpoint_requires_ip_port() {
    validate_endpoint_id("192.168.1.10:8080").expect("endpoint");
    assert!(validate_endpoint_id("localhost:8080").is_err());
    assert!(validate_endpoint_id("192.168.1.10").is_err());
}

#[test]
fn set_validate_and_expand() {
    let root = repo_root();
    let set = validate_service_set_file(&root, Path::new("sets/distributed-oj.yaml"))
        .expect("distributed oj set");
    let expanded = expand_set(&set);
    assert!(expanded.services.contains(&"gateway".to_string()));
    assert!(!expanded.default_links.is_empty());
}

#[test]
fn checked_in_sets_reference_existing_services() {
    let root = repo_root();
    let mut actual = relative_files(&root, &root.join("sets"))
        .into_iter()
        .filter(|path| path.ends_with(".yaml"))
        .collect::<Vec<_>>();
    actual.sort();
    assert_eq!(
        actual,
        vec![
            "sets/course-judge.yaml",
            "sets/distributed-oj.yaml",
            "sets/judge-worker-node.yaml",
            "sets/service-development.yaml",
            "sets/single-node-oj.yaml",
        ],
        "only the five formal Set files should remain"
    );

    for entry in fs::read_dir(root.join("sets")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|value| value.to_str()) != Some("yaml") {
            continue;
        }
        let rel = Path::new("sets").join(entry.file_name());
        let set = validate_service_set_file(&root, &rel)
            .unwrap_or_else(|err| panic!("{} should validate: {err}", rel.display()));
        validate_service_set_references(&root, &set)
            .unwrap_or_else(|err| panic!("{} references should validate: {err}", rel.display()));
    }
}

#[test]
fn set_rejects_missing_service_references() {
    let root = repo_root();
    let mut set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
    set.services
        .push(ServiceSetService::Id("missing-service".to_string()));
    assert!(validate_service_set_references(&root, &set).is_err());
}

#[test]
fn service_install_operation_uses_operation_model() {
    let manifest = valid_service();
    let operation =
        service_install_operation("op-service-install", &manifest, &[]).expect("install operation");
    assert_eq!(operation.status, OperationStatus::Planned);
    assert_eq!(operation.action, "service.install");
    assert_eq!(operation.target_type, "Service");
    assert_eq!(operation.target_id, "demo-api");
    assert!(
        operation
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|steps| steps.iter().any(|item| item
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("insert_service")))
    );
}

#[test]
fn core_plans_service_set_endpoint_link_and_topology_operations() {
    let root = repo_root();
    let set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml"))
        .expect("single node set");
    let set_operation = set_apply_operation("op-set-apply", &set).expect("set apply operation");
    assert_eq!(set_operation.action, "set.apply");
    assert_eq!(set_operation.target_type, "Set");
    assert!(
        set_operation
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|steps| steps.iter().any(|item| item
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("declare_link"))),
        "set.apply should plan default links"
    );

    let lifecycle =
        service_lifecycle_operation("op-service-restart", "service.restart", "judge-worker")
            .expect("service restart operation");
    assert_eq!(lifecycle.action, "service.restart");
    assert_eq!(lifecycle.target_type, "Service");
    assert_eq!(lifecycle.target_id, "judge-worker");

    let endpoints = vec![
        Endpoint {
            endpoint: "192.168.1.10:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
        Endpoint {
            endpoint: "192.168.1.10:8082".to_string(),
            service_id: "judge-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Judge API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let endpoint_operation = endpoint_register_operation("op-endpoint-register", &endpoints[0])
        .expect("endpoint register operation");
    assert_eq!(endpoint_operation.action, "endpoint.register");
    assert_eq!(endpoint_operation.target_type, "Endpoint");
    assert_eq!(endpoint_operation.target_id, "192.168.1.10:8080");
    let endpoint_update =
        endpoint_update_operation("op-endpoint-update", &endpoints[0]).expect("endpoint update");
    assert_eq!(endpoint_update.action, "endpoint.update");
    let endpoint_delete = endpoint_delete_operation("op-endpoint-delete", "192.168.1.10:8080")
        .expect("endpoint delete");
    assert_eq!(endpoint_delete.action, "endpoint.delete");
    let endpoint_health =
        endpoint_health_check_operation("op-endpoint-health", "192.168.1.10:8080")
            .expect("endpoint health");
    assert_eq!(endpoint_health.action, "endpoint.health.check");

    let link = Link {
        source_endpoint: "192.168.1.10:8080".to_string(),
        target_endpoint: "192.168.1.10:8082".to_string(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "api".to_string(),
        health: "ok".to_string(),
        latency_ms: Some(2),
        config_ref: "config://gateway/judge-api".to_string(),
        secret_ref: "secret://gateway/judge-api".to_string(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let link_operation =
        link_create_operation("op-link-create", &link, &endpoints).expect("link create operation");
    assert_eq!(link_operation.action, "link.create");
    assert_eq!(link_operation.target_type, "Link");
    assert_eq!(
        link_operation.target_id,
        "192.168.1.10:8080 -> 192.168.1.10:8082"
    );
    let link_update =
        link_update_operation("op-link-update", &link, &endpoints).expect("link update");
    assert_eq!(link_update.action, "link.update");
    let link_delete = link_delete_operation("op-link-delete", &link).expect("link delete");
    assert_eq!(link_delete.action, "link.delete");
    let link_health = link_health_check_operation("op-link-health", &link).expect("link health");
    assert_eq!(link_health.action, "link.health.check");

    let service_health =
        service_health_check_operation("op-service-health", "judge-api", Some("192.168.1.10:8082"))
            .expect("service health operation");
    assert_eq!(service_health.action, "service.health.check");
    let service_logs =
        service_logs_view_operation("op-service-logs", "judge-api", Some("192.168.1.10:8082"))
            .expect("service logs operation");
    assert_eq!(service_logs.action, "service.logs.view");

    let topology = build_topology(
        "192.168.1.10:8080".to_string(),
        vec!["gateway".to_string(), "judge-api".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints,
        vec![link],
        vec![],
        vec![],
        vec![],
    )
    .expect("topology");
    let topology_operation =
        topology_apply_operation("op-topology-apply", &topology).expect("topology apply");
    assert_eq!(topology_operation.action, "topology.apply");
    assert_eq!(topology_operation.target_type, "Topology");
    assert_eq!(topology_operation.target_id, "192.168.1.10:8080");
}

#[test]
fn action_registry_contains_required_actions_and_no_forbidden_actions() {
    let root = repo_root();
    let shared_schemas = load_shared_schemas(&root).expect("shared schemas should load");
    ensure_shared_schemas_loaded(&shared_schemas).expect("shared schemas should be complete");
    let text = fs::read_to_string(root.join("orchestrator/schemas/actions.yaml")).unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    let actions = action_set(&root);
    assert!(
        value.get("forbidden_prefixes").is_none(),
        "actions.yaml should only define shared GUI/TUI actions"
    );

    for required in [
        "deployment.create",
        "deployment.open",
        "deployment.diagnose",
        "service.import",
        "service.validate",
        "service.install",
        "service.enable",
        "service.disable",
        "service.start",
        "service.stop",
        "service.restart",
        "service.delete",
        "service.logs.view",
        "service.health.check",
        "set.import",
        "set.validate",
        "set.expand",
        "set.apply",
        "set.compare",
        "endpoint.register",
        "endpoint.update",
        "endpoint.delete",
        "endpoint.health.check",
        "link.create",
        "link.update",
        "link.delete",
        "link.health.check",
        "topology.load",
        "topology.validate",
        "topology.apply",
        "topology.export",
        "operation.plan",
        "operation.confirm",
        "operation.apply",
        "operation.cancel",
        "operation.rollback",
        "operation.logs.view",
        "diagnostics.run",
        "diagnostics.export",
    ] {
        assert!(actions.contains(required), "missing action {required}");
    }

    for action in actions {
        assert!(
            FORMAL_ACTION_PREFIXES
                .iter()
                .any(|prefix| action.starts_with(*prefix)),
            "action uses a non-formal orchestrator prefix: {action}"
        );
    }
}

#[test]
fn core_action_catalog_covers_registry_and_core_objects() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    let descriptors =
        validate_action_catalog(&schemas).expect("core action catalog should match schemas");
    let schema_actions = action_set(&root);
    let descriptor_actions = descriptors
        .iter()
        .map(|descriptor| descriptor.action.to_string())
        .collect::<HashSet<_>>();
    assert_eq!(schema_actions, descriptor_actions);
    assert_eq!(descriptors.len(), 39);

    for descriptor in &descriptors {
        assert!(
            CORE_ACTION_TARGETS.contains(&descriptor.target_type),
            "{} should target a formal core object",
            descriptor.action
        );
        assert!(
            FORMAL_ACTION_PREFIXES
                .iter()
                .any(|prefix| descriptor.action.starts_with(*prefix)),
            "{} should use a formal action prefix",
            descriptor.action
        );
        if descriptor.risk == ActionRisk::High {
            assert!(
                descriptor.plan_mode.requires_confirmation(),
                "{} is high risk and must require confirmation",
                descriptor.action
            );
        }
    }

    assert_eq!(
        action_descriptor("service.install").map(|item| item.target_type),
        Some("Service")
    );
    assert_eq!(
        action_descriptor("service.install").map(|item| item.plan_mode),
        Some(ActionPlanMode::ConfirmedPlan)
    );
    assert_eq!(
        action_descriptor("deployment.create").map(|item| item.target_type),
        Some("Topology"),
        "deployment action is mapped to Topology and does not introduce a new core object"
    );
}

#[test]
fn default_action_requests_cover_catalog_required_fields() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    let descriptors = validate_action_catalog(&schemas).expect("action catalog");
    for descriptor in descriptors {
        let request = default_action_request(descriptor.action)
            .unwrap_or_else(|| panic!("missing default request for {}", descriptor.action));
        assert_eq!(request.action, descriptor.action);
        let form = schemas
            .form_for(descriptor.action)
            .unwrap_or_else(|| panic!("missing form for {}", descriptor.action));
        for field in &form.fields {
            if field.required {
                assert!(
                    request.field(&field.name).is_some(),
                    "{} default request missing required field {}",
                    descriptor.action,
                    field.name
                );
            }
        }
    }
}

#[test]
fn service_lifecycle_plans_follow_action_catalog_confirmation_rules() {
    for action in [
        "service.enable",
        "service.disable",
        "service.start",
        "service.stop",
        "service.restart",
        "service.delete",
    ] {
        let operation = service_lifecycle_operation(
            format!("op-{action}").replace('.', "-"),
            action,
            "judge-worker",
        )
        .expect("service lifecycle operation");
        let descriptor = action_descriptor(action).expect("action descriptor");
        let requires_confirmation = operation
            .plan
            .get("requires_confirmation")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        assert_eq!(
            requires_confirmation,
            descriptor.plan_mode.requires_confirmation(),
            "{action} plan should match Action Catalog confirmation rule"
        );
    }
}

#[test]
fn action_request_planner_creates_operation_previews() {
    let root = repo_root();
    let services = [
        "services/gateway/service.yaml",
        "services/problem-api/service.yaml",
    ]
    .into_iter()
    .map(|path| validate_service_manifest_file(&root, Path::new(path)).unwrap())
    .collect::<Vec<_>>();
    let set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
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
            config: serde_json::json!({}),
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
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints.clone(),
        vec![Link {
            source_endpoint: "127.0.0.1:8080".to_string(),
            target_endpoint: "127.0.0.1:8081".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "oj".to_string(),
            health: "ok".to_string(),
            latency_ms: Some(1),
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();

    let install_request = ActionRequest::new(
        "op-preview-service-install",
        "service.install",
        [("service_id".to_string(), "gateway".to_string())]
            .into_iter()
            .collect(),
    );
    let install_preview = plan_action_preview(
        &install_request,
        &services,
        std::slice::from_ref(&set),
        &endpoints,
        Some(&topology),
    )
    .expect("service install preview");
    assert_eq!(install_preview.target_type, "Service");
    assert_eq!(install_preview.target_id, "gateway");
    assert!(install_preview.requires_confirmation);
    assert!(
        install_preview
            .steps
            .iter()
            .any(|step| step == "refresh_service_metadata")
    );

    let link_request = ActionRequest::new(
        "op-preview-link-create",
        "link.create",
        [
            ("source_endpoint".to_string(), "127.0.0.1:8080".to_string()),
            ("target_endpoint".to_string(), "127.0.0.1:8081".to_string()),
            ("protocol".to_string(), "http".to_string()),
        ]
        .into_iter()
        .collect(),
    );
    let link_operation = plan_action_request(
        &link_request,
        &services,
        &[set],
        &endpoints,
        Some(&topology),
    )
    .expect("link create operation");
    assert_eq!(link_operation.action, "link.create");
    assert_eq!(link_operation.target_type, "Link");
}

#[test]
fn operation_workbench_builds_preview_for_every_action() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    let descriptors = validate_action_catalog(&schemas).expect("action catalog");
    let services = [
        "services/gateway/service.yaml",
        "services/problem-api/service.yaml",
    ]
    .into_iter()
    .map(|path| validate_service_manifest_file(&root, Path::new(path)).unwrap())
    .collect::<Vec<_>>();
    let set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
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
            config: serde_json::json!({}),
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
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints.clone(),
        vec![Link {
            source_endpoint: "127.0.0.1:8080".to_string(),
            target_endpoint: "127.0.0.1:8081".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "oj".to_string(),
            health: "ok".to_string(),
            latency_ms: Some(1),
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();

    for descriptor in descriptors {
        let workbench = build_operation_workbench(
            descriptor.action,
            &schemas,
            &services,
            std::slice::from_ref(&set),
            &endpoints,
            Some(&topology),
        )
        .unwrap_or_else(|err| panic!("{} should build workbench: {err}", descriptor.action));
        assert_eq!(workbench.selected_action, descriptor.action);
        assert_eq!(workbench.request.action, descriptor.action);
        assert_eq!(workbench.preview.action, descriptor.action);
        assert_eq!(workbench.operation.action, descriptor.action);
        assert!(workbench.required_fields_satisfied);
        assert_eq!(
            workbench.can_confirm, workbench.preview.requires_confirmation,
            "{} confirmation should come from core preview",
            descriptor.action
        );
        assert!(
            !workbench
                .form_fields
                .iter()
                .any(|field| field.name.trim().is_empty())
        );
    }
}

#[test]
fn operation_workbench_runs_confirm_apply_and_rollback_flow() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    let services = vec![
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap(),
    ];
    let set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
    let endpoints = vec![Endpoint {
        endpoint: "127.0.0.1:8080".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: "Gateway".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }];
    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints.clone(),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let workbench = build_operation_workbench(
        "service.install",
        &schemas,
        &services,
        &[set],
        &endpoints,
        Some(&topology),
    )
    .expect("workbench");

    assert!(workbench.preview.requires_confirmation);
    let run = run_operation_workbench_flow(&workbench).expect("workbench run");
    assert_eq!(run.planned_status, OperationStatus::Planned);
    assert_eq!(
        run.confirmed_status,
        Some(OperationStatus::AwaitingConfirmation)
    );
    assert_eq!(run.applied_status, OperationStatus::Succeeded);
    assert_eq!(run.rolled_back_status, Some(OperationStatus::RolledBack));
    assert_eq!(run.result_status, "SUCCEEDED");
    assert!(
        run.logs.iter().any(|record| !record.step_id.is_empty()),
        "workbench apply should preserve persisted step logs"
    );
}

#[test]
fn operation_workbench_updates_fields_and_runs_step_by_step() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    let services = vec![
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap(),
        validate_service_manifest_file(&root, Path::new("services/problem-api/service.yaml"))
            .unwrap(),
    ];
    let set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
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
            config: serde_json::json!({}),
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
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints.clone(),
        vec![Link {
            source_endpoint: "127.0.0.1:8080".to_string(),
            target_endpoint: "127.0.0.1:8081".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "oj".to_string(),
            health: "ok".to_string(),
            latency_ms: Some(1),
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();

    let workbench = build_operation_workbench(
        "service.install",
        &schemas,
        &services,
        std::slice::from_ref(&set),
        &endpoints,
        Some(&topology),
    )
    .expect("workbench");
    let session = new_operation_workbench_session(workbench);
    let updated = update_operation_workbench_field(
        &session,
        "service_id",
        "problem-api",
        &schemas,
        &services,
        std::slice::from_ref(&set),
        &endpoints,
        Some(&topology),
    )
    .expect("updated workbench");
    assert_eq!(
        updated.workbench.request.field("service_id"),
        Some("problem-api")
    );
    assert_eq!(updated.workbench.preview.target_id, "problem-api");

    assert!(
        apply_operation_workbench_session(&updated).is_err(),
        "confirmed plan action must not apply before confirmation"
    );
    let confirmed = confirm_operation_workbench_session(&updated).expect("confirm");
    assert_eq!(
        confirmed.current_operation.status,
        OperationStatus::AwaitingConfirmation
    );
    let applied = apply_operation_workbench_session(&confirmed).expect("apply");
    assert_eq!(applied.current_operation.status, OperationStatus::Succeeded);
    assert_eq!(applied.result_status, "SUCCEEDED");
    assert!(
        applied.logs.iter().any(|record| !record.step_id.is_empty()),
        "apply should write step logs"
    );
    let rolled_back = rollback_operation_workbench_session(&applied).expect("rollback");
    assert_eq!(
        rolled_back.current_operation.status,
        OperationStatus::RolledBack
    );
    assert_eq!(rolled_back.result_status, "ROLLED_BACK");
    assert!(rolled_back.logs.len() > applied.logs.len());
}

#[test]
fn operation_workbench_context_loads_repo_and_drives_sessions() {
    let root = repo_root();
    let context = load_operation_workbench_context(&root).expect("workbench context");

    assert_eq!(context.schemas.action_count(), 39);
    assert_eq!(context.services.len(), 9);
    assert_eq!(context.sets.len(), 5);
    assert!(!context.endpoints.is_empty());
    assert!(!context.links.is_empty());
    assert!(context.topology.is_some());

    for descriptor in validate_action_catalog(&context.schemas).expect("action catalog") {
        context
            .build_session(descriptor.action)
            .unwrap_or_else(|err| panic!("{} should build from context: {err}", descriptor.action));
    }

    let session = context
        .build_session("service.install")
        .expect("service install session");
    let updated = context
        .update_field(&session, "service_id", "problem-api")
        .expect("field update should rebuild through core context");
    assert_eq!(
        updated.workbench.request.field("service_id"),
        Some("problem-api")
    );
    assert_eq!(updated.workbench.preview.target_id, "problem-api");

    let service_field = updated
        .workbench
        .form_fields
        .iter()
        .find(|field| field.name == "service_id")
        .expect("service_id field");
    let suggested = context.suggested_field_values(service_field);
    assert!(suggested.contains(&"gateway".to_string()));
    assert!(suggested.contains(&"problem-api".to_string()));

    let cycled = context
        .cycle_field_value(&updated, "service_id")
        .expect("cycle service field");
    assert!(cycled.workbench.request.field("service_id").is_some());

    let confirmed = context.confirm(&updated).expect("confirm");
    let applied = context.apply(&confirmed).expect("apply");
    assert_eq!(applied.current_operation.status, OperationStatus::Succeeded);
    assert_eq!(applied.result_status, "SUCCEEDED");
    let rolled_back = context.rollback(&applied).expect("rollback");
    assert_eq!(
        rolled_back.current_operation.status,
        OperationStatus::RolledBack
    );
    assert_eq!(rolled_back.result_status, "ROLLED_BACK");
}

#[test]
fn operation_workbench_context_applies_store_backed_core_actions() {
    let root = repo_root();
    let context = load_operation_workbench_context(&root).expect("workbench context");

    let endpoint_session = context
        .build_session("endpoint.register")
        .expect("endpoint register session");
    let endpoint_applied = context
        .apply(&endpoint_session)
        .expect("endpoint register should use context store");
    assert_eq!(
        endpoint_applied.current_operation.status,
        OperationStatus::Succeeded
    );
    assert_eq!(endpoint_applied.result_status, "SUCCEEDED");

    let link_session = context
        .build_session("link.create")
        .expect("link create session");
    let link_confirmed = context.confirm(&link_session).expect("confirm link create");
    let link_applied = context
        .apply(&link_confirmed)
        .expect("link create should find endpoints from context store");
    assert_eq!(
        link_applied.current_operation.status,
        OperationStatus::Succeeded
    );
    assert_eq!(link_applied.result_status, "SUCCEEDED");

    let set_session = context
        .build_session("set.apply")
        .expect("set apply session");
    let set_confirmed = context.confirm(&set_session).expect("confirm set apply");
    let set_applied = context
        .apply(&set_confirmed)
        .expect("set apply should find services from context store");
    assert_eq!(
        set_applied.current_operation.status,
        OperationStatus::Succeeded
    );
    assert_eq!(set_applied.result_status, "SUCCEEDED");
}

#[test]
fn shared_schemas_cover_gui_tui_contract() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    ensure_shared_schemas_loaded(&schemas).expect("shared schemas should be usable");

    assert_eq!(schemas.actions.len(), 39);
    assert_eq!(schemas.actions.len(), schemas.form_actions.len());
    assert_eq!(schemas.actions.len(), schemas.forms.len());
    assert!(
        schemas
            .form_for("service.install")
            .is_some_and(
                |form| form.fields.iter().any(|field| field.name == "service_id"
                    && field.field_type == "service_id"
                    && field.required)
            ),
        "service.install should expose required service_id form field"
    );
    assert!(
        schemas
            .form_for("topology.load")
            .is_some_and(|form| form.fields.is_empty()),
        "topology.load should be represented as an empty form"
    );
    for state in [
        "PLANNED",
        "AWAITING_CONFIRMATION",
        "RUNNING",
        "SUCCEEDED",
        "FAILED",
        "ROLLED_BACK",
        "CANCELLED",
        "EXPIRED",
    ] {
        assert!(
            schemas.plan_states.iter().any(|item| item == state),
            "missing operation state {state}"
        );
    }
    for object in [
        "Service",
        "Set",
        "Endpoint",
        "Link",
        "Operation",
        "Topology",
        "LogView",
        "DiagnosticReport",
    ] {
        assert!(
            schemas
                .result_object_types
                .iter()
                .any(|item| item == object),
            "missing changed object type {object}"
        );
    }
    for sensitive in ["token", "secret", "password", "private_key"] {
        assert!(
            schemas
                .error_redactions
                .iter()
                .any(|item| item == sensitive),
            "missing error redaction marker {sensitive}"
        );
    }
}

#[test]
fn shared_view_pages_cover_core_objects_for_gui_and_tui() {
    let titles = OrchestratorViewPage::all()
        .iter()
        .map(|page| page.title())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec![
            "总览",
            "Service",
            "Set",
            "Endpoint",
            "Link",
            "Operation",
            "Topology",
            "LogView",
            "DiagnosticReport",
        ]
    );

    let objects = OrchestratorViewPage::all()
        .iter()
        .filter_map(|page| page.core_object())
        .collect::<HashSet<_>>();
    for object in [
        "Service",
        "Set",
        "Endpoint",
        "Link",
        "Operation",
        "Topology",
        "LogView",
        "DiagnosticReport",
    ] {
        assert!(objects.contains(object), "missing view page for {object}");
    }

    let keys = OrchestratorViewPage::all()
        .iter()
        .filter_map(|page| page.key())
        .collect::<HashSet<_>>();
    assert_eq!(keys.len(), OrchestratorViewPage::all().len());
}

#[test]
fn formal_docs_tree_contains_only_rewritten_docs() {
    let root = repo_root();
    let docs = relative_files(&root, &root.join("docs"));
    assert_eq!(
        docs,
        vec![
            "docs/architecture/README.md",
            "docs/orchestrator/action-model.md",
            "docs/orchestrator/boundary.md",
            "docs/orchestrator/database.md",
            "docs/orchestrator/gui-tui-parity.md",
            "docs/orchestrator/operation-model.md",
            "docs/orchestrator/requirements.md",
            "docs/orchestrator/topology-model.md",
            "docs/release/README.md",
            "docs/release/evidence.md",
            "docs/services/README.md",
            "docs/spec/endpoint-link-spec.md",
            "docs/spec/service-spec.md",
            "docs/spec/set-spec.md",
        ],
        "docs/ should contain only rewritten formal Orchestrator documents"
    );

    let docs_temp_count = relative_files(&root, &root.join("docs-temp")).len();
    assert!(
        docs_temp_count > 0,
        "docs-temp/ should retain historical documents outside the formal docs tree"
    );
}

#[test]
fn retired_entry_directories_and_empty_placeholders_are_absent() {
    let root = repo_root();
    for path in [
        concat!("run", "time"),
        concat!("inst", "aller"),
        concat!("scr", "ipts"),
        concat!("shared", "-", "ui"),
    ] {
        assert!(
            !root.join(path).exists(),
            "{path} should not be restored as a formal product path"
        );
    }
    assert!(
        !root.join(concat!("kernel/", "inst", "aller")).exists(),
        "retired kernel implementation path should not be restored"
    );

    let placeholders = relative_files(&root, &root)
        .into_iter()
        .filter(|path| {
            path.ends_with(".gitkeep")
                && !path.starts_with(".git/")
                && !path.contains("/target/")
                && !path.contains("/node_modules/")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        placeholders,
        Vec::<String>::new(),
        ".gitkeep placeholders should not be used as architecture"
    );

    let script_files = relative_files(&root, &root)
        .into_iter()
        .filter(|path| {
            (path.ends_with(".ps1")
                || path.ends_with(".sh")
                || path.ends_with(".bat")
                || path.ends_with(".cmd"))
                && !path.starts_with(".git/")
                && !path.contains("/target/")
                && !path.contains("/node_modules/")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        script_files,
        Vec::<String>::new(),
        "script files should not be retained as product or acceptance surface"
    );
}

#[test]
fn form_registry_covers_every_action() {
    let root = repo_root();
    let forms_text = fs::read_to_string(root.join("orchestrator/schemas/forms.yaml")).unwrap();
    let forms_value: serde_yaml::Value = serde_yaml::from_str(&forms_text).unwrap();
    let actions = action_set(&root);
    let forms = forms_value
        .get("forms")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("forms should be a mapping");
    let form_keys = forms
        .keys()
        .map(|item| {
            item.as_str()
                .expect("form key should be a string")
                .to_string()
        })
        .collect::<HashSet<_>>();

    assert_eq!(
        actions, form_keys,
        "forms.yaml must cover actions.yaml exactly"
    );

    for (action, form) in forms {
        let action = action.as_str().unwrap();
        let fields = form
            .get("fields")
            .and_then(serde_yaml::Value::as_sequence)
            .unwrap_or_else(|| panic!("{action} fields should be a sequence"));
        for field in fields {
            let name = field
                .get("name")
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or("");
            assert!(
                !name.contains('.'),
                "form field must be local: {action}.{name}"
            );
        }
    }
}

#[test]
fn plan_result_and_error_schemas_cover_core_objects() {
    let root = repo_root();
    let forms_text = fs::read_to_string(root.join("orchestrator/schemas/forms.yaml")).unwrap();
    let plans_text = fs::read_to_string(root.join("orchestrator/schemas/plans.yaml")).unwrap();
    let results_text = fs::read_to_string(root.join("orchestrator/schemas/results.yaml")).unwrap();
    let errors_text = fs::read_to_string(root.join("orchestrator/schemas/errors.yaml")).unwrap();
    let forms: serde_yaml::Value = serde_yaml::from_str(&forms_text).unwrap();
    let plans: serde_yaml::Value = serde_yaml::from_str(&plans_text).unwrap();
    let results: serde_yaml::Value = serde_yaml::from_str(&results_text).unwrap();
    let errors: serde_yaml::Value = serde_yaml::from_str(&errors_text).unwrap();
    let core_objects = [
        "Service",
        "Set",
        "Endpoint",
        "Link",
        "Operation",
        "Topology",
        "LogView",
        "DiagnosticReport",
    ];

    for action in ["operation.plan", "diagnostics.run"] {
        let target_types = forms
            .get("forms")
            .and_then(|forms| forms.get(action))
            .and_then(|form| form.get("fields"))
            .and_then(serde_yaml::Value::as_sequence)
            .and_then(|fields| {
                fields.iter().find(|field| {
                    field.get("name").and_then(serde_yaml::Value::as_str) == Some("target_type")
                })
            })
            .and_then(|field| field.get("values"))
            .and_then(serde_yaml::Value::as_sequence)
            .map(|items| string_set(items.as_slice()))
            .unwrap_or_else(|| panic!("{action} target_type values should be a sequence"));
        for object in core_objects {
            assert!(
                target_types.contains(object),
                "{action} target_type missing core object {object}"
            );
        }
    }

    let plan_fields = string_set(
        plans
            .get("plan")
            .and_then(|plan| plan.get("required_fields"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("plan required_fields should be a sequence"),
    );
    for field in [
        "operation_id",
        "action",
        "target_type",
        "target_id",
        "steps",
        "risk_level",
        "rollback_available",
    ] {
        assert!(plan_fields.contains(field), "plans.yaml missing {field}");
    }

    let plan_states = string_set(
        plans
            .get("plan")
            .and_then(|plan| plan.get("states"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("plan states should be a sequence"),
    );
    for state in [
        "PLANNED",
        "AWAITING_CONFIRMATION",
        "RUNNING",
        "SUCCEEDED",
        "FAILED",
        "ROLLED_BACK",
        "CANCELLED",
        "EXPIRED",
    ] {
        assert!(plan_states.contains(state), "plans.yaml missing {state}");
    }

    let result_types = string_set(
        results
            .get("result")
            .and_then(|result| result.get("changed_object_types"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("result changed_object_types should be a sequence"),
    );
    for object in core_objects {
        assert!(
            result_types.contains(object),
            "results.yaml missing core object {object}"
        );
    }

    let error_redaction = string_set(
        errors
            .get("error")
            .and_then(|error| error.get("redaction"))
            .and_then(|redaction| redaction.get("forbidden_plaintext"))
            .and_then(serde_yaml::Value::as_sequence)
            .expect("error redaction forbidden_plaintext should be a sequence"),
    );
    for sensitive in ["token", "secret", "password", "private_key"] {
        assert!(
            error_redaction.contains(sensitive),
            "errors.yaml should redact {sensitive}"
        );
    }
}

fn string_set(items: &[serde_yaml::Value]) -> HashSet<String> {
    items
        .iter()
        .map(|item| {
            item.as_str()
                .expect("schema item should be a string")
                .to_string()
        })
        .collect()
}

#[test]
fn orchestrator_view_loads_all_core_object_rows() {
    let root = repo_root();
    let view = load_orchestrator_view(&root).expect("load orchestrator view");
    ensure_view_is_loaded(&view).expect("view should contain core data");

    assert_eq!(view.services.len(), 9);
    assert_eq!(view.schemas.action_count(), view.operations.len());
    assert_eq!(view.schemas.action_count(), view.schemas.form_count());
    assert_eq!(view.sets.len(), 5);
    assert!(!view.endpoints.is_empty());
    assert!(!view.links.is_empty());
    assert!(!view.operations.is_empty());
    assert!(!view.logs.is_empty());
    let workbench = view
        .operation_workbench
        .as_ref()
        .expect("view should expose shared operation workbench");
    assert_eq!(workbench.selected_action, "service.install");
    assert_eq!(workbench.target, "Service gateway");
    assert!(workbench.fields.contains("service_id*"));
    assert!(!workbench.preview_steps.is_empty());
    assert!(
        view.operations
            .iter()
            .any(|item| item.action == "service.install"
                && item.target == "Service"
                && item.risk == "高"
                && item.mode == "计划并确认"
                && item.plan_required == "必须确认"
                && item.fields.contains("service_id*")),
        "view should expose core action catalog semantics"
    );
    assert!(
        view.operations
            .iter()
            .any(|item| item.action == "deployment.create"
                && item.target == "Topology"
                && item.summary.contains("拓扑")),
        "deployment actions should map to core objects instead of adding new core object types"
    );
    assert!(
        view.operations
            .iter()
            .all(|item| !item.preview_target.is_empty()
                && !item.preview_steps.is_empty()
                && !item.preview_confirmation.is_empty()),
        "every action row should expose a core-generated plan preview"
    );
    for endpoint in &view.endpoints {
        validate_endpoint_id(&endpoint.endpoint)
            .unwrap_or_else(|err| panic!("view endpoint should be IP:Port: {err}"));
    }
    for link in &view.links {
        validate_endpoint_id(&link.from)
            .unwrap_or_else(|err| panic!("view link source should be IP:Port: {err}"));
        validate_endpoint_id(&link.to)
            .unwrap_or_else(|err| panic!("view link target should be IP:Port: {err}"));
    }
    assert_eq!(
        view.diagnostics
            .iter()
            .find(|item| item.target == "judge-worker-node")
            .map(|item| item.status.as_str()),
        Some("ok")
    );
}

#[test]
fn topology_uses_endpoint_identity_without_machine_or_installation() {
    let endpoints = vec![
        Endpoint {
            endpoint: "192.168.1.10:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
        Endpoint {
            endpoint: "192.168.1.10:8083".to_string(),
            service_id: "problem-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let authority = topology_authority("192.168.1.10:8080")
        .expect("root authority should derive from endpoint");
    let topology = Topology {
        root_host: authority.root_host.clone(),
        root_endpoint: "192.168.1.10:8080".to_string(),
        authority,
        services: vec!["gateway".to_string(), "problem-api".to_string()],
        sets: vec!["single-node-oj".to_string()],
        endpoints,
        links: vec![Link {
            source_endpoint: "192.168.1.10:8080".to_string(),
            target_endpoint: "192.168.1.10:8083".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "oj".to_string(),
            health: "unknown".to_string(),
            latency_ms: None,
            config_ref: String::new(),
            secret_ref: "secret://gateway/problem-api".to_string(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }],
        operations: vec![Operation {
            operation_id: "op-1".to_string(),
            action: "topology.validate".to_string(),
            target_type: "Topology".to_string(),
            target_id: "current".to_string(),
            status: OperationStatus::Planned,
            request: serde_json::json!({}),
            plan: serde_json::json!({}),
            result: serde_json::json!({}),
            error_message: String::new(),
            rollback_plan: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
            confirmed_at: String::new(),
            started_at: String::new(),
            finished_at: String::new(),
            rolled_back_at: String::new(),
        }],
        log_views: vec![LogView {
            source_id: "gateway:health".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "192.168.1.10:8080".to_string(),
            operation_id: String::new(),
            path: "/health".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Gateway health".to_string(),
        }],
        diagnostic_reports: vec![DiagnosticReport {
            report_id: "diag-1".to_string(),
            target_type: "Topology".to_string(),
            target_id: "current".to_string(),
            status: "ok".to_string(),
            summary: "拓扑合法".to_string(),
            operation_id: String::new(),
            data: serde_json::json!({}),
            findings: vec![DiagnosticFinding {
                code: "topology.valid".to_string(),
                severity: "info".to_string(),
                message: "Endpoint 和 Link 均可核对".to_string(),
                redacted: false,
            }],
            created_at: String::new(),
        }],
    };

    validate_topology(&topology).expect("topology should use Endpoint as runtime identity");
}

#[test]
fn topology_rejects_root_host_that_does_not_match_root_endpoint() {
    let authority = topology_authority("192.168.1.10:8080")
        .expect("root authority should derive from endpoint");
    let topology = Topology {
        root_host: "192.168.1.20".to_string(),
        root_endpoint: "192.168.1.10:8080".to_string(),
        authority,
        services: vec!["gateway".to_string()],
        sets: vec![],
        endpoints: vec![Endpoint {
            endpoint: "192.168.1.10:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: String::new(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }],
        links: vec![],
        operations: vec![],
        log_views: vec![],
        diagnostic_reports: vec![],
    };

    assert!(validate_topology(&topology).is_err());
}

#[test]
fn topology_builder_validates_endpoint_and_link_identity() {
    let endpoints = vec![
        Endpoint {
            endpoint: "192.168.1.10:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: String::new(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
        Endpoint {
            endpoint: "192.168.1.10:8082".to_string(),
            service_id: "judge-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: String::new(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let links = vec![Link {
        source_endpoint: "192.168.1.10:8080".to_string(),
        target_endpoint: "192.168.1.10:8082".to_string(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "api".to_string(),
        health: "ok".to_string(),
        latency_ms: Some(2),
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }];

    let topology = build_topology(
        "192.168.1.10:8080".to_string(),
        vec!["gateway".to_string(), "judge-api".to_string()],
        vec!["single-node-oj".to_string()],
        endpoints,
        links,
        vec![],
        vec![],
        vec![],
    )
    .expect("topology builder should validate endpoint/link identity");

    assert_eq!(topology.root_endpoint, "192.168.1.10:8080");
    assert_eq!(topology.root_host, "192.168.1.10");
    assert_eq!(topology.authority.root_endpoint, topology.root_endpoint);
    assert_eq!(topology.authority.root_host, topology.root_host);
    assert_eq!(topology.links.len(), 1);
}

#[test]
fn topology_rejects_unknown_link_endpoint() {
    let endpoints = vec![Endpoint {
        endpoint: "192.168.1.10:8080".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: String::new(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }];
    let link = Link {
        source_endpoint: "192.168.1.10:8080".to_string(),
        target_endpoint: "192.168.1.20:9101".to_string(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: String::new(),
        health: String::new(),
        latency_ms: None,
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };

    assert!(validate_link(&link, &endpoints).is_err());
}

#[test]
fn secret_text_is_redacted_for_operation_errors() {
    assert_eq!(redact_secret_text("worker token leaked"), "<redacted>");
    assert_eq!(
        redact_secret_text("ordinary diagnostic"),
        "ordinary diagnostic"
    );

    let log = operation_log_record("op-redact", "warn", "password leaked in diagnostic");
    assert_eq!(log.message, "<redacted>");
    assert!(log.redacted);
}

#[test]
fn operation_lifecycle_plans_confirms_applies_and_rolls_back() {
    let planned = plan_operation(
        "op-1",
        "topology.apply",
        "Topology",
        "current",
        serde_json::json!({"reason": "test"}),
        serde_json::json!({"steps": ["validate", "apply"]}),
        serde_json::json!({"steps": ["restore"]}),
    )
    .expect("plan operation");
    assert_eq!(planned.status, OperationStatus::Planned);

    let confirmed = confirm_operation(&planned).expect("confirm operation");
    assert_eq!(confirmed.status, OperationStatus::AwaitingConfirmation);

    let cancelled = cancel_operation(&confirmed).expect("cancel operation");
    assert_eq!(cancelled.status, OperationStatus::Cancelled);

    let expired = expire_operation(&planned).expect("expire operation");
    assert_eq!(expired.status, OperationStatus::Expired);

    let running = start_operation(&confirmed).expect("start operation");
    assert_eq!(running.status, OperationStatus::Running);

    let failed = fail_operation(&running, "worker token leaked").expect("fail operation");
    assert_eq!(failed.status, OperationStatus::Failed);
    assert_eq!(failed.error_message, "<redacted>");

    let rolled_back =
        rollback_operation(&failed, serde_json::json!({"restored": true})).expect("rollback");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
}

#[test]
fn memory_store_enforces_endpoint_and_link_boundaries() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    let mut judge_api = valid_service();
    judge_api.id = "judge-api".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(judge_api).expect("put judge api");

    let gateway_endpoint = Endpoint {
        endpoint: "192.168.1.10:8080".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: String::new(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let judge_endpoint = Endpoint {
        endpoint: "192.168.1.10:8082".to_string(),
        service_id: "judge-api".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: String::new(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    store
        .put_endpoint(gateway_endpoint)
        .expect("put gateway endpoint");
    store
        .put_endpoint(judge_endpoint)
        .expect("put judge api endpoint");

    store
        .put_link(Link {
            source_endpoint: "192.168.1.10:8080".to_string(),
            target_endpoint: "192.168.1.10:8082".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            health: "ok".to_string(),
            latency_ms: Some(1),
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("link uses registered endpoints");

    assert_eq!(store.endpoints().len(), 2);
    assert_eq!(store.links().len(), 1);
}

#[test]
fn operation_executor_applies_and_rolls_back_through_store() {
    let mut store = MemoryOrchestratorStore::new();
    let mut worker = valid_service();
    worker.id = "judge-worker".to_string();
    store.put_service(worker).expect("put judge-worker service");
    let operation = plan_operation(
        "op-apply-1",
        "service.restart",
        "Service",
        "judge-worker",
        serde_json::json!({}),
        serde_json::json!({"steps": ["stop", "start"]}),
        serde_json::json!({"steps": ["restore"]}),
    )
    .expect("plan operation");
    store.put_operation(operation).expect("put operation");

    assert!(
        OperationExecutor::new(&mut store)
            .apply("op-apply-1")
            .is_err(),
        "apply must require explicit confirmation"
    );

    let confirmed = confirm_operation(
        store
            .operation("op-apply-1")
            .expect("operation should exist"),
    )
    .expect("confirm operation");
    store
        .put_operation(confirmed)
        .expect("put confirmed operation");

    let applied = OperationExecutor::new(&mut store)
        .apply("op-apply-1")
        .expect("apply operation");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    assert_eq!(
        store.operation("op-apply-1").map(|item| &item.status),
        Some(&OperationStatus::Succeeded)
    );
    assert_eq!(
        applied
            .result
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("SUCCEEDED")
    );
    assert_eq!(
        applied
            .result
            .get("changed_objects")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert!(
        store
            .operation_logs("op-apply-1")
            .iter()
            .any(|record| !record.step_id.is_empty()),
        "apply should write step logs"
    );

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-apply-1")
        .expect("rollback operation");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    assert_eq!(
        store.operation("op-apply-1").map(|item| &item.status),
        Some(&OperationStatus::RolledBack)
    );
    assert_eq!(
        rolled_back
            .result
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("ROLLED_BACK")
    );
    assert!(
        store
            .operation_logs("op-apply-1")
            .iter()
            .any(|record| record.message.contains("rolled back"))
    );
}

#[test]
fn log_query_reads_only_scoped_sources_and_operation_logs() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
    store
        .put_log_view(LogView {
            source_id: "gateway:service".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "127.0.0.1:8080".to_string(),
            operation_id: String::new(),
            path: "/logs".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Gateway logs".to_string(),
        })
        .expect("put scoped log view");
    store
        .put_operation(
            service_logs_view_operation("op-log-query", "gateway", Some("127.0.0.1:8080"))
                .expect("service log operation"),
        )
        .expect("put operation");
    store
        .append_operation_log(operation_step_log_record(
            "op-log-query",
            "step-1",
            "info",
            "service log collected",
            serde_json::json!({"service_id": "gateway"}),
        ))
        .expect("append operation log");

    let result = query_logs(
        &store,
        &LogQuery {
            service_id: Some("gateway".to_string()),
            endpoint: Some("127.0.0.1:8080".to_string()),
            operation_id: Some("op-log-query".to_string()),
            source_id: None,
        },
    )
    .expect("query logs");
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.operation_logs.len(), 1);
    assert_eq!(result.operation_logs[0].step_id, "step-1");

    assert!(
        store
            .put_log_view(LogView {
                source_id: "bad".to_string(),
                service_id: "gateway".to_string(),
                endpoint: "127.0.0.1:8080".to_string(),
                operation_id: String::new(),
                path: "../host.log".to_string(),
                driver: "external-endpoint".to_string(),
                read_policy: "service-scoped".to_string(),
                display_name: String::new(),
            })
            .is_err(),
        "LogView must not become an arbitrary file browser"
    );
    assert!(
        validate_log_view(&LogView {
            source_id: "bad-endpoint".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "gateway.local".to_string(),
            operation_id: String::new(),
            path: "/logs".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: String::new(),
        })
        .is_err(),
        "LogView endpoint identity must remain IP:Port"
    );
}

#[test]
fn operation_executor_allows_planned_apply_when_confirmation_is_not_required() {
    let mut store = MemoryOrchestratorStore::new();
    let mut worker = valid_service();
    worker.id = "judge-worker".to_string();
    store.put_service(worker).expect("put judge-worker service");
    let operation =
        service_lifecycle_operation("op-start-1", "service.start", "judge-worker").unwrap();
    assert_eq!(
        operation
            .plan
            .get("requires_confirmation")
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    store.put_operation(operation).expect("put operation");

    let applied = OperationExecutor::new(&mut store)
        .apply("op-start-1")
        .expect("non-dangerous planned operation can apply directly");
    assert_eq!(applied.status, OperationStatus::Succeeded);
}

#[test]
fn operation_executor_mutates_core_store_objects() {
    let root = repo_root();
    let mut store = MemoryOrchestratorStore::new();
    let gateway =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let problem_api =
        validate_service_manifest_file(&root, Path::new("services/problem-api/service.yaml"))
            .unwrap();

    let install = service_install_operation("op-install-gateway", &gateway, &[])
        .expect("service install operation");
    let install = confirm_operation(&install).expect("confirm install");
    store.put_operation(install).expect("put install");
    OperationExecutor::new(&mut store)
        .apply("op-install-gateway")
        .expect("apply install");
    assert!(store.service("gateway").is_some());

    store
        .put_service(problem_api.clone())
        .expect("put problem-api service");
    let gateway_endpoint = Endpoint {
        endpoint: "127.0.0.1:8080".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Gateway".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let endpoint_op = endpoint_register_operation("op-endpoint", &gateway_endpoint)
        .expect("endpoint register operation");
    store.put_operation(endpoint_op).expect("put endpoint op");
    OperationExecutor::new(&mut store)
        .apply("op-endpoint")
        .expect("apply endpoint");
    assert!(store.endpoint("127.0.0.1:8080").is_some());

    let problem_endpoint = Endpoint {
        endpoint: "127.0.0.1:8081".to_string(),
        service_id: "problem-api".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Problem API".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    store
        .put_endpoint(problem_endpoint)
        .expect("put problem endpoint");
    let link = Link {
        source_endpoint: "127.0.0.1:8080".to_string(),
        target_endpoint: "127.0.0.1:8081".to_string(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "oj".to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let link_op = link_create_operation("op-link", &link, &store.endpoints()).expect("link op");
    let link_op = confirm_operation(&link_op).expect("confirm link");
    store.put_operation(link_op).expect("put link op");
    OperationExecutor::new(&mut store)
        .apply("op-link")
        .expect("apply link");
    assert_eq!(store.links().len(), 1);

    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        vec!["single-node-oj".to_string()],
        store.endpoints(),
        store.links(),
        Vec::new(),
        vec![LogView {
            source_id: "gateway:health".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "127.0.0.1:8080".to_string(),
            operation_id: String::new(),
            path: "/health".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Gateway health".to_string(),
        }],
        vec![DiagnosticReport {
            report_id: "diag-topology".to_string(),
            target_type: "Topology".to_string(),
            target_id: "127.0.0.1:8080".to_string(),
            status: "ok".to_string(),
            summary: "拓扑可用".to_string(),
            operation_id: String::new(),
            data: serde_json::json!({}),
            findings: Vec::new(),
            created_at: String::new(),
        }],
    )
    .expect("topology");
    let topology_op = topology_apply_operation("op-topology", &topology).expect("topology op");
    let topology_op = confirm_operation(&topology_op).expect("confirm topology");
    store.put_operation(topology_op).expect("put topology op");
    OperationExecutor::new(&mut store)
        .apply("op-topology")
        .expect("apply topology");
    assert!(store.topology("127.0.0.1:8080").is_some());
    assert_eq!(store.log_views().len(), 1);
    assert_eq!(store.diagnostic_reports().len(), 1);
}

#[test]
fn orchestrator_database_migration_contains_only_formal_tables() {
    let root = repo_root();
    let sql = fs::read_to_string(
        root.join("deploy/orchestrator-migrations/000001_orchestrator_schema.up.sql"),
    )
    .expect("orchestrator migration");
    let report = inspect_orchestrator_schema(&sql).expect("schema should be service-first");

    for table in ORCHESTRATOR_TABLES {
        assert!(
            report.tables.iter().any(|item| item == table),
            "missing formal table {table}"
        );
    }
    assert!(
        sql.contains("rollback_plan JSONB NOT NULL DEFAULT '{}'::jsonb"),
        "orchestrator_operations must persist rollback_plan"
    );
    assert!(
        sql.contains("snapshot_id TEXT PRIMARY KEY")
            && sql.contains("topology JSONB NOT NULL")
            && sql.contains("created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()"),
        "topology_snapshots must persist snapshot_id, topology, and created_at"
    );
    assert!(
        report.non_formal_tables.is_empty(),
        "non-formal tables should not appear: {:?}",
        report.non_formal_tables
    );
}

#[test]
fn compose_separates_orchestrator_and_oj_databases() {
    let root = repo_root();
    let compose = fs::read_to_string(root.join("deploy/compose/docker-compose.yml"))
        .expect("compose file should exist");
    let value: serde_yaml::Value =
        serde_yaml::from_str(compose.trim_start_matches('\u{feff}')).expect("compose should parse");
    let services = value
        .get("services")
        .and_then(serde_yaml::Value::as_mapping)
        .expect("compose services should be a mapping");

    for service_name in [
        "orchestrator-db",
        "postgres",
        "orchestrator-migrations",
        "oj-migrations",
    ] {
        assert!(
            services
                .keys()
                .any(|key| key.as_str() == Some(service_name)),
            "compose missing {service_name}"
        );
    }

    let orchestrator_migrations = services
        .get(serde_yaml::Value::String(
            "orchestrator-migrations".to_string(),
        ))
        .expect("orchestrator-migrations service");
    assert!(
        yaml_text(orchestrator_migrations).contains("../orchestrator-migrations:/migrations:ro"),
        "orchestrator migrations must mount only deploy/orchestrator-migrations"
    );
    assert!(
        yaml_text(orchestrator_migrations).contains("ORCHESTRATOR_DATABASE_URL"),
        "orchestrator migrations must use ORCHESTRATOR_DATABASE_URL"
    );

    let oj_migrations = services
        .get(serde_yaml::Value::String("oj-migrations".to_string()))
        .expect("oj-migrations service");
    assert!(
        yaml_text(oj_migrations).contains("../oj-migrations:/migrations:ro"),
        "OJ migrations must mount only deploy/oj-migrations"
    );
    assert!(
        yaml_text(oj_migrations).contains("OJ_DATABASE_URL"),
        "OJ migrations must use OJ_DATABASE_URL"
    );

    for service_name in ["gateway", "auth", "problem-api", "judge-api"] {
        let service = services
            .get(serde_yaml::Value::String(service_name.to_string()))
            .unwrap_or_else(|| panic!("compose missing service {service_name}"));
        let text = yaml_text(service);
        assert!(
            text.contains("OJ_DATABASE_URL"),
            "{service_name} must use OJ_DATABASE_URL for business data"
        );
        assert!(
            !text.contains("ORCHESTRATOR_DATABASE_URL"),
            "{service_name} must not receive ORCHESTRATOR_DATABASE_URL"
        );
    }
}

#[test]
fn oj_migrations_do_not_create_orchestrator_tables() {
    let root = repo_root();
    let forbidden_table_patterns = ORCHESTRATOR_TABLES
        .iter()
        .flat_map(|table| {
            [
                format!("create table if not exists {table}"),
                format!("create table {table}"),
                format!("alter table {table}"),
                format!("insert into {table}"),
                format!("update {table}"),
                format!("delete from {table}"),
            ]
        })
        .collect::<Vec<_>>();
    let forbidden_permission_patterns = [
        "'service.install'",
        "'service.enable'",
        "'service.disable'",
        "'service.configure'",
        "'launcher.view'",
        "'launcher.install'",
        "'launcher.uninstall'",
        "'launcher.enable'",
        "'launcher.disable'",
        "'service_manager'",
    ];

    for entry in fs::read_dir(root.join("deploy/oj-migrations")).expect("oj migrations") {
        let entry = entry.expect("migration entry");
        if entry.path().extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }
        let sql = fs::read_to_string(entry.path()).expect("read oj migration");
        let lowered = sql.to_lowercase();
        for item in &forbidden_table_patterns {
            assert!(
                !lowered.contains(item),
                "{} must not create or write orchestrator table pattern {item}",
                entry.file_name().to_string_lossy()
            );
        }
        for item in forbidden_permission_patterns {
            assert!(
                !lowered.contains(item),
                "{} must not seed orchestrator or launcher permission {item}",
                entry.file_name().to_string_lossy()
            );
        }
    }
}

#[test]
fn orchestrator_database_access_touches_only_formal_tables() {
    let report = inspect_database_access(ORCHESTRATOR_DATABASE_STATEMENTS)
        .expect("database access should stay inside orchestrator schema");

    assert!(!report.touched_tables.is_empty());
    assert_eq!(report.missing_tables, Vec::<String>::new());
    assert_eq!(report.non_formal_tables, Vec::<String>::new());
    for table in ORCHESTRATOR_TABLES {
        assert!(
            report.touched_tables.iter().any(|item| item == table),
            "database access missing formal table {table}"
        );
    }
}

#[test]
fn database_write_plan_maps_store_objects_to_formal_tables() {
    let root = repo_root();
    let mut store = MemoryOrchestratorStore::new();
    let gateway =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let problem_api =
        validate_service_manifest_file(&root, Path::new("services/problem-api/service.yaml"))
            .unwrap();
    let set = validate_service_set_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
    store.put_service(gateway).expect("put service");
    store.put_service(problem_api).expect("put problem api");
    store.put_set(set).expect("put set");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8083".to_string(),
            service_id: "problem-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put problem endpoint");
    store
        .put_link(Link {
            source_endpoint: "127.0.0.1:8080".to_string(),
            target_endpoint: "127.0.0.1:8083".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            health: "ok".to_string(),
            latency_ms: Some(1),
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put link");
    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        vec!["single-node-oj".to_string()],
        store.endpoints(),
        store.links(),
        Vec::new(),
        vec![LogView {
            source_id: "gateway:health".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "127.0.0.1:8080".to_string(),
            operation_id: String::new(),
            path: "/health".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Gateway health".to_string(),
        }],
        vec![DiagnosticReport {
            report_id: "diag-gateway".to_string(),
            target_type: "Service".to_string(),
            target_id: "gateway".to_string(),
            status: "ok".to_string(),
            summary: "Service 可观测".to_string(),
            operation_id: String::new(),
            data: serde_json::json!({}),
            findings: Vec::new(),
            created_at: String::new(),
        }],
    )
    .expect("topology");
    let operation =
        service_health_check_operation("op-health-gateway", "gateway", Some("127.0.0.1:8080"))
            .expect("health operation");
    store.put_operation(operation).expect("put operation");
    store
        .append_operation_log(operation_log_record(
            "op-health-gateway",
            "info",
            "health checked",
        ))
        .expect("append log");
    store.put_topology(topology.clone()).expect("put topology");
    for log_view in topology.log_views {
        store.put_log_view(log_view).expect("put log view");
    }
    for report in topology.diagnostic_reports {
        store
            .put_diagnostic_report(report)
            .expect("put diagnostic report");
    }

    let plan = plan_database_writes(&store).expect("database write plan");
    assert_eq!(plan.non_formal_tables, Vec::<String>::new());
    for table in ORCHESTRATOR_TABLES {
        assert!(
            plan.touched_tables.iter().any(|item| item == table),
            "write plan should include formal table {table}"
        );
    }
    for (object_type, table) in [
        ("Service", "services"),
        ("Set", "service_sets"),
        ("Endpoint", "service_endpoints"),
        ("Operation", "orchestrator_operations"),
        ("OperationLog", "orchestrator_operation_logs"),
        ("Topology", "topology_snapshots"),
        ("LogView", "log_sources"),
        ("DiagnosticReport", "diagnostic_reports"),
    ] {
        assert!(
            plan.writes
                .iter()
                .any(|write| write.object_type == object_type && write.table == table),
            "{object_type} should map to {table}"
        );
    }
}

fn yaml_text(value: &serde_yaml::Value) -> String {
    serde_yaml::to_string(value).expect("yaml value should render")
}

#[test]
fn pg_orchestrator_store_uses_only_orchestrator_database_url() {
    let store =
        PgOrchestratorStore::new("postgres://postgres:local@localhost:5432/ojos_orchestrator")
            .expect("pg store should accept orchestrator database url");
    assert_eq!(PgOrchestratorStore::ENV_NAME, "ORCHESTRATOR_DATABASE_URL");
    assert!(
        !store.database_url().contains("OJ_DATABASE_URL"),
        "PgOrchestratorStore must not point at the OJ business database"
    );
    assert!(
        store
            .statements()
            .iter()
            .all(|statement| !statement.sql.contains("module_"))
    );
    let log_statement = store
        .statements()
        .iter()
        .find(|statement| statement.name == "log_sources.upsert")
        .expect("log source statement should exist");
    assert!(
        log_statement.sql.contains("operation_id"),
        "Pg LogView persistence must preserve operation-scoped log sources"
    );
}

#[test]
fn pg_orchestrator_lock_statement_accepts_session_style_locks() {
    let store =
        PgOrchestratorStore::new("postgres://postgres:local@localhost:5432/ojos_orchestrator")
            .expect("pg store should accept orchestrator database url");
    let lock_statement = store
        .statements()
        .iter()
        .find(|statement| statement.name == "orchestrator_operation_locks.acquire")
        .expect("lock acquire statement should exist");

    assert!(
        lock_statement
            .sql
            .contains("COALESCE(NULLIF($4, '')::TIMESTAMPTZ"),
        "empty or non-persistent lock expiration should fall back to a DB-side expiry"
    );
    assert!(
        lock_statement.sql.contains("INTERVAL '5 minutes'"),
        "session-style operation locks should not be cast directly as timestamps"
    );
}

#[test]
fn memory_store_lock_and_step_logs_are_persisted_by_executor() {
    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "judge-worker".to_string();
    store.put_service(service).expect("put service");
    let operation =
        service_lifecycle_operation("op-lock-log", "service.restart", "judge-worker").unwrap();
    let operation = confirm_operation(&operation).expect("confirm");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::new(&mut store)
        .apply("op-lock-log")
        .expect("apply operation");
    let logs = store.operation_logs("op-lock-log");
    assert!(
        logs.iter().any(|record| !record.step_id.is_empty()),
        "apply should persist step logs"
    );
    assert_eq!(
        store.operation("op-lock-log").map(|item| &item.status),
        Some(&OperationStatus::Succeeded)
    );
}

#[test]
fn fixed_executor_drivers_reject_arbitrary_actions() {
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:8080".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Gateway".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let request = driver_request_for_endpoint("service.start", &endpoint);
    assert!(
        LocalProcessDriver::new().execute(&request).is_err(),
        "local process driver should not start processes without a supervisor binding"
    );

    let compose = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
    let command = compose
        .command_for("service.restart", "gateway")
        .expect("fixed docker compose command");
    assert_eq!(command[0], "docker");
    assert!(command.contains(&"restart".to_string()));
    assert!(compose.command_for("service.shell", "gateway").is_err());

    let external = ExternalEndpointDriver::default();
    let health = driver_request_for_endpoint("endpoint.health.check", &endpoint);
    assert_eq!(
        external.execute(&health).expect("external health").status,
        "SUPPORTED"
    );
    assert!(external.execute(&request).is_err());
}

#[test]
fn endpoint_and_link_health_checks_return_formal_statuses() {
    let source = Endpoint {
        endpoint: "127.0.0.1:8080".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: "Gateway".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let target = Endpoint {
        endpoint: "127.0.0.1:8083".to_string(),
        service_id: "problem-api".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "healthy".to_string(),
        reachable: true,
        display_name: "Problem API".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let result = check_endpoint_health_with_probe(&target, &StaticEndpointProbe)
        .expect("static health check");
    assert_eq!(result.health, "healthy");
    assert!(result.reachable);

    let link = Link {
        source_endpoint: source.endpoint.clone(),
        target_endpoint: target.endpoint.clone(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "api".to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let link_result =
        check_link_health(&link, &[source, target], &result).expect("link health check");
    assert_eq!(link_result.health, "healthy");
}

#[test]
fn tcp_probe_checks_http_health_path_status() {
    let endpoint = local_http_endpoint(
        "/health",
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
    );
    let result =
        check_endpoint_health_with_probe(&endpoint, &TcpEndpointProbe::new(Duration::from_secs(2)))
            .expect("http health check");
    assert_eq!(result.health, "healthy");
    assert!(result.reachable);
    assert_eq!(result.message, "http 204");
}

#[test]
fn tcp_probe_marks_http_non_success_status_as_degraded() {
    let endpoint = local_http_endpoint(
        "health",
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
    );
    let result =
        check_endpoint_health_with_probe(&endpoint, &TcpEndpointProbe::new(Duration::from_secs(2)))
            .expect("http health check");
    assert_eq!(result.health, "degraded");
    assert!(result.reachable);
    assert_eq!(result.message, "http 500");
}

#[test]
fn orchestrator_view_can_load_from_store_state() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("schemas");
    let mut store = MemoryOrchestratorStore::new();
    let gateway =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    store.put_service(gateway).expect("service");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("endpoint");
    let operation =
        service_health_check_operation("op-store-view", "gateway", Some("127.0.0.1:8080"))
            .expect("operation");
    store.put_operation(operation).expect("operation");
    let view = load_orchestrator_view_from_store(schemas, &store).expect("store view");
    assert_eq!(view.services.len(), 1);
    assert_eq!(view.endpoints[0].endpoint, "127.0.0.1:8080");
    assert_eq!(view.operations[0].action, "service.health.check");
}

#[test]
fn diagnostic_report_json_exports_observable_summary() {
    let topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string()],
        vec!["single-node-oj".to_string()],
        vec![Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        }],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("topology");
    let report = diagnostic_report_json(&topology).expect("diagnostic report json");
    assert!(report.contains("services_summary"));
    assert!(report.contains("database_schema_check"));
    assert!(report.contains("forbidden_concept_scan_summary"));
}

#[test]
fn diagnostic_report_builds_from_store_and_exports_json_and_markdown() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "unreachable".to_string(),
            reachable: false,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
    let failed = plan_operation(
        "op-failed",
        "service.restart",
        "Service",
        "gateway",
        serde_json::json!({}),
        serde_json::json!({"steps": ["restart"]}),
        serde_json::json!({"steps": ["restore"]}),
    )
    .and_then(|operation| start_operation(&operation))
    .and_then(|operation| fail_operation(&operation, "restart failed"))
    .expect("failed operation");
    store.put_operation(failed).expect("put failed operation");
    store
        .append_operation_log(operation_step_log_record(
            "op-failed",
            "restart",
            "error",
            "restart failed",
            serde_json::json!({"endpoint": "127.0.0.1:8080"}),
        ))
        .expect("append operation log");

    let topology = store.build_topology_view().expect("topology");
    store.put_topology(topology).expect("put topology");
    let report = build_diagnostic_report(&store, "diag-current").expect("diagnostic report");
    assert_eq!(report.status, "degraded");
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "operations.failed")
    );
    assert!(
        report
            .data
            .get("recent_operation_logs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.get("operation_id")
                == Some(&serde_json::Value::String("op-failed".to_string()))))
    );

    let json_export = export_diagnostic_report(&report, "json").expect("json export");
    assert!(json_export.content.contains("diag-current"));
    let markdown_export = export_diagnostic_report(&report, "markdown").expect("markdown export");
    assert!(
        markdown_export
            .content
            .contains("# DiagnosticReport diag-current")
    );
    assert!(export_diagnostic_report(&report, "html").is_err());
}

#[test]
fn reconcile_tick_refreshes_health_topology_and_diagnostics() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    let mut problem_api = valid_service();
    problem_api.id = "problem-api".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put gateway endpoint");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8083".to_string(),
            service_id: "problem-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put problem endpoint");
    store
        .put_link(Link {
            source_endpoint: "127.0.0.1:8080".to_string(),
            target_endpoint: "127.0.0.1:8083".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            health: "unknown".to_string(),
            latency_ms: None,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put link");
    let mut waiting = plan_operation(
        "op-stale",
        "service.restart",
        "Service",
        "gateway",
        serde_json::json!({}),
        serde_json::json!({"steps": ["restart"]}),
        serde_json::json!({"steps": ["restore"]}),
    )
    .expect("operation");
    waiting.status = OperationStatus::AwaitingConfirmation;
    waiting.confirmed_at = String::new();
    store.put_operation(waiting).expect("put operation");

    let result = run_reconcile_tick(&mut store, &StaticEndpointProbe, "unit")
        .expect("reconcile tick should run");
    assert_eq!(result.expired_operations, vec!["op-stale"]);
    assert_eq!(result.checked_endpoints.len(), 2);
    assert_eq!(
        store
            .operation("op-stale")
            .map(|operation| &operation.status),
        Some(&OperationStatus::Expired)
    );
    assert_eq!(
        store
            .endpoint("127.0.0.1:8080")
            .map(|endpoint| endpoint.health.as_str()),
        Some("healthy")
    );
    assert_eq!(
        store.links().first().map(|link| link.health.as_str()),
        Some("healthy")
    );
    assert_eq!(result.topology_snapshot_id, Some("tick-unit".to_string()));
    assert_eq!(
        result.diagnostic_report_id,
        Some("diag-tick-unit".to_string())
    );
    assert!(
        store
            .diagnostic_reports()
            .iter()
            .any(|report| report.report_id == "diag-tick-unit")
    );
}

#[test]
fn reconcile_tick_snapshot_uses_refreshed_store_state() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    let mut problem_api = valid_service();
    problem_api.id = "problem-api".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put gateway endpoint");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8083".to_string(),
            service_id: "problem-api".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "ok".to_string(),
            reachable: true,
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put problem endpoint");
    store
        .put_link(Link {
            source_endpoint: "127.0.0.1:8080".to_string(),
            target_endpoint: "127.0.0.1:8083".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            health: "unknown".to_string(),
            latency_ms: None,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put link");
    let stale_topology = build_topology(
        "127.0.0.1:8080".to_string(),
        vec!["gateway".to_string(), "problem-api".to_string()],
        Vec::new(),
        store.endpoints(),
        store.links(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("stale topology");
    store
        .put_topology(stale_topology)
        .expect("put stale topology");

    run_reconcile_tick(&mut store, &StaticEndpointProbe, "fresh")
        .expect("reconcile tick should run");

    let latest = store
        .get_latest_topology_snapshot()
        .expect("latest topology")
        .expect("snapshot exists");
    assert_eq!(latest.snapshot_id, "tick-fresh");
    assert!(
        latest
            .topology
            .endpoints
            .iter()
            .all(|endpoint| endpoint.health == "healthy")
    );
    assert_eq!(
        latest
            .topology
            .links
            .first()
            .map(|link| link.health.as_str()),
        Some("healthy")
    );
}

fn local_http_endpoint(health_path: &str, response: &'static str) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local http listener");
    let endpoint = listener.local_addr().expect("local addr").to_string();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept health request");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });

    Endpoint {
        endpoint,
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: health_path.to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Local HTTP".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }
}
