use crate::*;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn valid_manifest() -> Manifest {
    Manifest {
        schema_version: 1,
        id: "ojos.demo".to_string(),
        name: "Demo Module".to_string(),
        version: "0.1.0".to_string(),
        set: "demo".to_string(),
        kind: "feature".to_string(),
        status: "demo".to_string(),
        description: "Installer validation demo module.".to_string(),
        compatibility: Compatibility {
            platform: ">=0.1.0".to_string(),
            installer: ">=0.1.0".to_string(),
        },
        requires: manifest::Requires {
            modules: vec![ModuleDependency {
                id: "ojos.platform.identity-access".to_string(),
                version: ">=0.1.0".to_string(),
            }],
        },
        provides: Provides {
            permissions: vec![PermissionDecl {
                key: "demo.view".to_string(),
                description: "View demo.".to_string(),
            }],
            roles: vec![RoleDecl {
                key: "demo.viewer".to_string(),
                description: "Demo viewer.".to_string(),
            }],
            components: vec![ComponentDecl {
                id: "demo-component".to_string(),
                component_type: "metadata".to_string(),
                status: "DISABLED".to_string(),
                config: serde_json::json!({}),
            }],
            services: vec![],
            workers: vec![],
            frontend_routes: vec![FrontendRouteDecl {
                path: "/admin/modules/demo".to_string(),
                name: "demo-module".to_string(),
                component_key: "demo-placeholder".to_string(),
                required_permission: "demo.view".to_string(),
                enabled: false,
            }],
            menus: vec![MenuDecl {
                key: "demo-module".to_string(),
                title: "Demo Module".to_string(),
                route_path: "/admin/modules/demo".to_string(),
                icon: String::new(),
                parent_key: String::new(),
                sort_order: 100,
                required_permission: "demo.view".to_string(),
                enabled: false,
            }],
            gateway_routes: vec![],
            storage: StorageDecl { buckets: vec![] },
            storage_buckets: vec![StorageBucketDecl {
                id: "demo-metadata".to_string(),
                description: "Demo metadata bucket declaration.".to_string(),
            }],
            health_checks: vec![HealthCheckDecl {
                id: "demo-health".to_string(),
                check_type: "metadata".to_string(),
                optional: true,
            }],
            migrations: vec![],
            events: EventsDecl {
                publishes: vec!["demo.installed".to_string()],
                subscribes: vec![],
            },
            scheduled_jobs: vec![],
            admin_panels: vec![AdminPanelDecl {
                id: "demo-panel".to_string(),
                route_path: "/admin/modules/demo".to_string(),
                required_permission: "demo.view".to_string(),
            }],
            topology: TopologyDecl {
                nodes: vec![TopologyNodeDecl {
                    id: "demo-component".to_string(),
                    node_type: "metadata".to_string(),
                    label: "Demo Component".to_string(),
                }],
                edges: vec![],
            },
        },
        signature: None,
        signing_key_id: None,
        trusted_publisher: None,
    }
}

fn snapshot() -> RegistrySnapshot {
    RegistrySnapshot {
        modules: vec![
            InstalledModule {
                module_id: "ojos.platform.identity-access".to_string(),
                name: "Identity Access".to_string(),
                version: "0.1.0".to_string(),
                status: ModuleState::Enabled,
                kind: "platform".to_string(),
                manifest: None,
            },
            InstalledModule {
                module_id: "ojos.judge-core".to_string(),
                name: "Judge Core".to_string(),
                version: "0.1.0".to_string(),
                status: ModuleState::Enabled,
                kind: "feature".to_string(),
                manifest: Some(Manifest {
                    id: "ojos.judge-core".to_string(),
                    status: "builtin".to_string(),
                    kind: "feature".to_string(),
                    set: "core".to_string(),
                    name: "Judge Core".to_string(),
                    version: "0.1.0".to_string(),
                    ..valid_manifest()
                }),
            },
        ],
    }
}

#[test]
fn valid_manifest_parse() {
    validate_manifest(&valid_manifest()).expect("manifest should be valid");
}

#[test]
fn invalid_schema_version() {
    let mut manifest = valid_manifest();
    manifest.schema_version = 2;
    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn invalid_id_and_semver() {
    let mut manifest = valid_manifest();
    manifest.id = "Bad_ID".to_string();
    assert!(validate_manifest(&manifest).is_err());
    let mut manifest = valid_manifest();
    manifest.version = "latest".to_string();
    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn duplicate_permission_and_gateway_prefix() {
    let mut manifest = valid_manifest();
    manifest
        .provides
        .permissions
        .push(manifest.provides.permissions[0].clone());
    assert!(validate_manifest(&manifest).is_err());

    let mut manifest = valid_manifest();
    manifest.provides.gateway_routes = vec![
        GatewayRouteDecl {
            prefix: "/api/demo".to_string(),
            target_service: "demo".to_string(),
            auth_mode: "required".to_string(),
            enabled: true,
        },
        GatewayRouteDecl {
            prefix: "/api/demo".to_string(),
            target_service: "demo2".to_string(),
            auth_mode: "required".to_string(),
            enabled: true,
        },
    ];
    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn service_and_worker_runtime_contract_validates() {
    let mut manifest = valid_manifest();
    manifest.provides.services = vec![ServiceDecl {
        id: "problem-api".to_string(),
        name: "Problem API".to_string(),
        kind: "http".to_string(),
        lifecycle: "managed".to_string(),
        trusted_runtime: "compose".to_string(),
        compose_service: "problem-api".to_string(),
        health_check_id: "problem-api-health".to_string(),
        routes: vec!["/api/problem".to_string()],
        required: true,
        path: "services/problem-api".to_string(),
        health: "/health".to_string(),
        exposure: "internal".to_string(),
    }];
    manifest.provides.workers = vec![WorkerDecl {
        id: "judge-worker".to_string(),
        name: "Judge Worker".to_string(),
        kind: "worker".to_string(),
        lifecycle: "managed".to_string(),
        trusted_runtime: "compose".to_string(),
        compose_service: "judge-worker".to_string(),
        health_check_id: "worker-cluster-health".to_string(),
        required: false,
        path: "services/judge-worker".to_string(),
        mode: "external-node".to_string(),
    }];

    validate_manifest(&manifest).expect("runtime service/worker contract should validate");
}

#[test]
fn duplicate_service_id_is_rejected() {
    let mut manifest = valid_manifest();
    manifest.provides.services = vec![
        ServiceDecl {
            id: "problem-api".to_string(),
            name: String::new(),
            kind: "http".to_string(),
            lifecycle: "managed".to_string(),
            trusted_runtime: "compose".to_string(),
            compose_service: "problem-api".to_string(),
            health_check_id: String::new(),
            routes: vec![],
            required: true,
            path: String::new(),
            health: String::new(),
            exposure: String::new(),
        },
        ServiceDecl {
            id: "problem-api".to_string(),
            name: String::new(),
            kind: "http".to_string(),
            lifecycle: "managed".to_string(),
            trusted_runtime: "compose".to_string(),
            compose_service: "problem-api-copy".to_string(),
            health_check_id: String::new(),
            routes: vec![],
            required: false,
            path: String::new(),
            health: String::new(),
            exposure: String::new(),
        },
    ];

    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn dangerous_runtime_fields_are_rejected() {
    for field in ["image", "host_path", "privileged", "cap_add", "command"] {
        let text = format!(
            r#"
schema_version: 1
id: ojos.demo
name: Demo
version: 0.1.0
set: demo
kind: feature
status: demo
provides:
  services:
    - id: demo-api
      lifecycle: managed
      trusted_runtime: compose
      compose_service: demo-api
      {field}: dangerous
"#
        );
        let parsed: Result<Manifest> = serde_yaml::from_str(&text).map_err(InstallerError::from);
        if let Ok(manifest) = parsed {
            assert!(
                validate_manifest(&manifest).is_err(),
                "dangerous field {field} should fail validation"
            );
        }
    }
}

#[test]
fn gateway_route_hotplug_auth_modes_are_valid() {
    for auth_mode in ["public", "user", "admin", "worker", "internal"] {
        let mut manifest = valid_manifest();
        manifest.provides.gateway_routes = vec![GatewayRouteDecl {
            prefix: format!("/api/demo-{}", auth_mode),
            target_service: "demo".to_string(),
            auth_mode: auth_mode.to_string(),
            enabled: auth_mode != "public",
        }];
        validate_manifest(&manifest).unwrap_or_else(|err| {
            panic!("auth mode {auth_mode} should be valid: {err}");
        });
    }
}

#[test]
fn gateway_route_service_id_alias_is_valid() {
    let yaml = r#"
schema_version: 1
id: ojos.demo
name: Demo
version: 0.1.0
set: demo
kind: feature
status: demo
provides:
  gateway_routes:
    - prefix: /api/demo
      service_id: demo-api
      auth_mode: user
      enabled: true
"#;
    let manifest: Manifest = serde_yaml::from_str(yaml).expect("manifest parses");
    assert_eq!(
        manifest.provides.gateway_routes[0].target_service,
        "demo-api"
    );
    validate_manifest(&manifest).expect("service_id alias should validate");
}

#[test]
fn gateway_route_direct_target_url_is_rejected() {
    let yaml = r#"
schema_version: 1
id: ojos.demo
name: Demo
version: 0.1.0
set: demo
kind: feature
status: demo
provides:
  gateway_routes:
    - prefix: /api/demo
      target_url: http://127.0.0.1:2375
      auth_mode: user
      enabled: true
"#;
    let err = serde_yaml::from_str::<Manifest>(yaml).expect_err("unknown target_url should fail");
    assert!(err.to_string().contains("target_url"));
}

#[test]
fn dangerous_fields_are_rejected() {
    let text = r#"
schema_version: 1
id: ojos.demo
name: Demo
version: 0.1.0
set: demo
kind: feature
status: demo
provides:
  components:
    - id: c
      type: metadata
      command: rm -rf /
"#;
    let parsed: Result<Manifest> = serde_yaml::from_str(text).map_err(InstallerError::from);
    assert!(parsed.is_err());
}

#[test]
fn manifest_path_rejects_traversal_absolute_and_tmp() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("modules/demo")).unwrap();
    fs::write(dir.path().join("modules/demo/module.yaml"), "x").unwrap();
    assert!(manifest::validate_manifest_path(dir.path(), Path::new("../module.yaml")).is_err());
    assert!(
        manifest::validate_manifest_path(dir.path(), Path::new("/modules/demo/module.yaml"))
            .is_err()
    );
    assert!(
        manifest::validate_manifest_path(dir.path(), Path::new("modules/.tmp/module.yaml"))
            .is_err()
    );
}

#[test]
fn validate_manifest_file_reads_from_repo_root() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("modules/demo")).unwrap();
    let text = serde_yaml::to_string(&valid_manifest()).unwrap();
    fs::write(dir.path().join("modules/demo/module.yaml"), text).unwrap();

    let other = tempdir().unwrap();
    let old_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(other.path()).unwrap();
    let result =
        manifest::validate_manifest_file(dir.path(), Path::new("modules/demo/module.yaml"));
    std::env::set_current_dir(old_cwd).unwrap();

    assert_eq!(result.unwrap().id, "ojos.demo");
}

#[test]
fn manifest_path_rejects_symlink_escape() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join("modules/demo")).unwrap();
    fs::write(dir.path().join("outside.yaml"), "x").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            dir.path().join("outside.yaml"),
            dir.path().join("modules/demo/module.yaml"),
        )
        .unwrap();
        assert!(
            manifest::validate_manifest_path(dir.path(), Path::new("modules/demo/module.yaml"))
                .is_err()
        );
    }
}

#[test]
fn dependency_errors_and_install_plan() {
    let mut manifest = valid_manifest();
    let plan = install_plan(&manifest, &snapshot(), true).unwrap();
    assert!(plan.can_apply);
    assert!(
        plan.actions
            .iter()
            .any(|a| a.action == "insert_module_metadata")
    );

    manifest.requires.modules[0].version = ">=9.0.0".to_string();
    let plan = install_plan(&manifest, &snapshot(), true).unwrap();
    assert!(!plan.can_apply);
    assert!(
        plan.blocked_by
            .iter()
            .any(|v| v.contains("version mismatch"))
    );

    manifest.requires.modules[0].id = "ojos.missing".to_string();
    manifest.requires.modules[0].version = ">=0.1.0".to_string();
    let plan = install_plan(&manifest, &snapshot(), true).unwrap();
    assert!(!plan.can_apply);
    assert!(
        plan.blocked_by
            .iter()
            .any(|v| v.contains("missing dependency"))
    );
}

#[test]
fn self_and_cycle_dependency_rejected() {
    let mut manifest = valid_manifest();
    manifest.requires.modules = vec![ModuleDependency {
        id: manifest.id.clone(),
        version: String::new(),
    }];
    assert!(validate_manifest(&manifest).is_err());

    let a = Manifest {
        id: "ojos.a".to_string(),
        requires: manifest::Requires {
            modules: vec![ModuleDependency {
                id: "ojos.b".to_string(),
                version: String::new(),
            }],
        },
        ..valid_manifest()
    };
    let b = Manifest {
        id: "ojos.b".to_string(),
        requires: manifest::Requires {
            modules: vec![ModuleDependency {
                id: "ojos.a".to_string(),
                version: String::new(),
            }],
        },
        ..valid_manifest()
    };
    let snap = RegistrySnapshot {
        modules: vec![InstalledModule {
            module_id: "ojos.b".to_string(),
            name: "B".to_string(),
            version: "0.1.0".to_string(),
            status: ModuleState::Enabled,
            kind: "feature".to_string(),
            manifest: Some(b),
        }],
    };
    let plan = install_plan(&a, &snap, true).unwrap();
    assert!(plan.blocked_by.iter().any(|v| v.contains("cycle")));
}

#[test]
fn disable_and_uninstall_protection() {
    let plan = disable_plan("ojos.judge-core", &snapshot(), true).unwrap();
    assert!(!plan.can_apply);
    assert!(plan.blocked_by.iter().any(|v| v.contains("judge-core")));

    let plan = uninstall_plan("ojos.judge-core", &snapshot(), true).unwrap();
    assert!(!plan.can_apply);
    assert!(plan.blocked_by.iter().any(|v| v.contains("protected")));

    let plan = disable_plan("ojos.platform.identity-access", &snapshot(), true).unwrap();
    assert!(!plan.can_apply);
    assert!(plan.blocked_by.iter().any(|v| v.contains("platform")));
}

#[test]
fn upgrade_and_rollback_plan() {
    let old = valid_manifest();
    let mut new = valid_manifest();
    new.version = "0.2.0".to_string();
    new.provides.permissions.push(PermissionDecl {
        key: "demo.edit".to_string(),
        description: String::new(),
    });
    let plan = upgrade_plan(Some(&old), &new, &snapshot(), true).unwrap();
    assert!(plan.actions.iter().any(|a| a.action == "add_permissions"));

    let plan = rollback_plan("ojos.judge-core", &snapshot(), true).unwrap();
    assert!(!plan.can_apply);
    assert!(plan.blocked_by.iter().any(|v| v.contains("protected")));
}

#[test]
fn package_checksum_verify_and_path_rejection() {
    let dir = tempdir().unwrap();
    let module_dir = dir.path().join("demo");
    fs::create_dir_all(&module_dir).unwrap();
    fs::write(
        module_dir.join("module.yaml"),
        serde_yaml::to_string(&valid_manifest()).unwrap(),
    )
    .unwrap();
    fs::write(module_dir.join("README.md"), "demo").unwrap();
    let package = dir.path().join("demo.ojosmod");
    let result = package_module(&module_dir, &package).unwrap();
    assert!(result.valid);
    let verified = verify_package(&package).unwrap();
    assert!(verified.files_checked >= 3);
    assert_eq!(verified.package.unwrap().format, "ojosmod");

    fs::write(module_dir.join(".env"), "SECRET=value").unwrap();
    assert!(package_module(&module_dir, &dir.path().join("bad.ojosmod")).is_err());
}

#[test]
fn package_requires_metadata() {
    let dir = tempdir().unwrap();
    let package = dir.path().join("missing-metadata.ojosmod");
    {
        let file = fs::File::create(&package).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        let manifest_text = serde_yaml::to_string(&valid_manifest()).unwrap();
        zip.start_file("module.yaml", options).unwrap();
        use std::io::Write;
        zip.write_all(manifest_text.as_bytes()).unwrap();
        let hash = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(manifest_text.as_bytes()))
        };
        zip.start_file("checksums.sha256", options).unwrap();
        zip.write_all(format!("{}  module.yaml\n", hash).as_bytes())
            .unwrap();
        zip.finish().unwrap();
    }
    assert!(verify_package(&package).is_err());
}
