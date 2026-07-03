use crate::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
#[cfg(windows)]
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

static DOCKER_BINARY_ENV_LOCK: Mutex<()> = Mutex::new(());

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone)]
struct RecordingAuthPermissionRegistrar {
    calls: Arc<Mutex<Vec<AuthPermissionRegistration>>>,
}

impl AuthPermissionRegistrar for RecordingAuthPermissionRegistrar {
    fn register_permissions(
        &self,
        request: &AuthPermissionRegistration,
    ) -> Result<AuthPermissionRegistrationResult> {
        self.calls
            .lock()
            .expect("auth permission registrar calls lock")
            .push(request.clone());
        Ok(AuthPermissionRegistrationResult {
            status: "registered".to_string(),
            message: "recorded by test registrar".to_string(),
            endpoint: "http://auth-service.test".to_string(),
            registered: request.permissions.len(),
        })
    }
}

#[derive(Clone)]
struct RecordingRedisResourceProvisioner {
    calls: Arc<Mutex<Vec<RedisProvisionRequest>>>,
}

impl RedisResourceProvisioner for RecordingRedisResourceProvisioner {
    fn provision_resources(&self, request: &RedisProvisionRequest) -> Result<RedisProvisionResult> {
        self.calls
            .lock()
            .expect("redis provisioner calls lock")
            .push(request.clone());
        Ok(RedisProvisionResult {
            status: "created".to_string(),
            message: "recorded by test redis provisioner".to_string(),
            endpoint: "127.0.0.1:6379".to_string(),
            provisioned: request
                .resources
                .iter()
                .map(|resource| RedisProvisionedResource {
                    name: resource.name.clone(),
                    kind: resource.kind.clone(),
                    stream: "ojos:judge:task".to_string(),
                    consumer_group: if resource.kind == "consumer-group" {
                        "judge-worker".to_string()
                    } else {
                        String::new()
                    },
                    status: "created".to_string(),
                })
                .collect(),
        })
    }
}

#[derive(Clone)]
struct RecordingStorageResourceProvisioner {
    calls: Arc<Mutex<Vec<StorageProvisionRequest>>>,
}

impl StorageResourceProvisioner for RecordingStorageResourceProvisioner {
    fn provision_resources(
        &self,
        request: &StorageProvisionRequest,
    ) -> Result<StorageProvisionResult> {
        self.calls
            .lock()
            .expect("storage provisioner calls lock")
            .push(request.clone());
        Ok(StorageProvisionResult {
            status: "ensured".to_string(),
            message: "recorded by test storage provisioner".to_string(),
            endpoint: "http://storage-service.test".to_string(),
            provisioned: request
                .resources
                .iter()
                .map(|resource| StorageProvisionedResource {
                    object_type: resource.object_type.clone(),
                    bucket: resource.bucket.clone(),
                    status: "ensured".to_string(),
                })
                .collect(),
        })
    }
}

#[derive(Clone)]
struct RecordingMigrationRunner {
    calls: Arc<Mutex<Vec<MigrationExecutionRequest>>>,
    result: MigrationExecutionResult,
}

impl MigrationRunner for RecordingMigrationRunner {
    fn execute_migrations(
        &self,
        request: &MigrationExecutionRequest,
    ) -> Result<MigrationExecutionResult> {
        self.calls
            .lock()
            .expect("migration runner calls lock")
            .push(request.clone());
        Ok(self.result.clone())
    }
}

#[derive(Clone)]
struct RecordingGatewayRoutePublisher {
    calls: Arc<Mutex<Vec<GatewayRoutePublishRequest>>>,
    result: GatewayRoutePublishResult,
}

impl GatewayRoutePublisher for RecordingGatewayRoutePublisher {
    fn publish_routes(
        &self,
        request: &GatewayRoutePublishRequest,
    ) -> Result<GatewayRoutePublishResult> {
        self.calls
            .lock()
            .expect("gateway route publisher calls lock")
            .push(request.clone());
        Ok(GatewayRoutePublishResult {
            route_count: if request.effective_routes.is_empty() {
                request.routes.len()
            } else {
                request.effective_routes.len()
            },
            ..self.result.clone()
        })
    }
}

#[derive(Clone)]
struct FailingGatewayRoutePublisher {
    message: String,
}

impl GatewayRoutePublisher for FailingGatewayRoutePublisher {
    fn publish_routes(
        &self,
        _request: &GatewayRoutePublishRequest,
    ) -> Result<GatewayRoutePublishResult> {
        Err(OrchestratorError::Dependency(self.message.clone()))
    }
}

#[derive(Clone)]
struct RecordingNodeServiceDispatcher {
    calls: Arc<Mutex<Vec<NodeServiceDispatchRequest>>>,
    result: NodeServiceDispatchResult,
}

impl NodeServiceDispatcher for RecordingNodeServiceDispatcher {
    fn dispatch_service(
        &self,
        request: &NodeServiceDispatchRequest,
    ) -> Result<NodeServiceDispatchResult> {
        self.calls
            .lock()
            .expect("node dispatcher calls lock")
            .push(request.clone());
        Ok(self.result.clone())
    }
}

#[derive(Clone)]
struct FailingMigrationRunner {
    calls: Arc<Mutex<Vec<MigrationExecutionRequest>>>,
    message: String,
}

impl MigrationRunner for FailingMigrationRunner {
    fn execute_migrations(
        &self,
        request: &MigrationExecutionRequest,
    ) -> Result<MigrationExecutionResult> {
        self.calls
            .lock()
            .expect("migration runner calls lock")
            .push(request.clone());
        Err(OrchestratorError::Dependency(self.message.clone()))
    }
}

fn repo_root() -> PathBuf {
    let mut current = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if current.join("Cargo.toml").is_file()
            && current
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

fn valid_release_for_service(service: &ServiceManifest) -> ServiceReleaseManifest {
    ServiceReleaseManifest {
        schema_version: 1,
        service_name: service.id.clone(),
        version: service.version.clone(),
        description: format!("{} release", service.name),
        service_type: service.kind.clone(),
        source: ReleaseSourceDecl {
            kind: "local".to_string(),
            url: format!("local://services/{}", service.id),
            checksum: String::new(),
        },
        runtime: ReleaseRuntimeDecl {
            kind: "image".to_string(),
            image: String::new(),
            binary: String::new(),
            system_service: String::new(),
            command: String::new(),
            args: Vec::new(),
            working_dir: String::new(),
            env: BTreeMap::new(),
        },
        frontend: ReleaseFrontendDecl::default(),
        backend: ReleaseBackendDecl {
            protocol: service.endpoint.protocol.clone(),
            port: service.endpoint.default_port,
            health_path: service.endpoint.health_path.clone(),
        },
        migrations: Vec::new(),
        apis: Vec::new(),
        permissions: service.permissions.clone(),
        routes: vec![ReleaseRouteDecl {
            path: format!("/api/{}/**", service.id),
            method: "ANY".to_string(),
            target_type: "endpoint-group".to_string(),
            target: format!("{}[*]", service.id),
            permission: "public".to_string(),
        }],
        redis: Vec::new(),
        storage: Vec::new(),
        dependencies: Vec::new(),
        required_apis: Vec::new(),
        service_identity: ReleaseServiceIdentityDecl::default(),
        config_schema: serde_json::json!({}),
        secrets: Vec::new(),
        observability: ReleaseObservabilityDecl::default(),
    }
}

fn seed_storage_identity_api_surfaces(store: &mut MemoryOrchestratorStore) {
    for (api_id, method, permission) in [
        ("storage.object.get", "GET", "storage.object.read"),
        ("storage.object.head", "HEAD", "storage.object.read"),
        ("storage.object.put", "PUT", "storage.object.write"),
    ] {
        store
            .upsert_service_api_surface(ServiceApiSurface {
                service_name: "storage-service".to_string(),
                version: "0.1.0".to_string(),
                api_id: api_id.to_string(),
                protocol: "http".to_string(),
                port_name: "http".to_string(),
                path_prefix: "/api/storage/objects".to_string(),
                methods: vec![method.to_string()],
                visibility: "descendants".to_string(),
                auth_mode: "service".to_string(),
                permission: permission.to_string(),
                stability: "stable".to_string(),
                api_version: "v1".to_string(),
                rate_limit: String::new(),
                timeout: String::new(),
                config: serde_json::json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            })
            .expect("put storage identity api surface");
    }
}

fn action_set(root: &Path) -> HashSet<String> {
    let text = fs::read_to_string(root.join("platform/schemas/orchestrator/actions.yaml")).unwrap();
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
                .to_str()
                .expect("file path should be valid UTF-8")
                .replace('\\', "/");
            files.push(rel);
        }
    }
}

#[test]
fn checked_in_service_manifests_validate() {
    let root = repo_root();
    let expected = [
        "services/auth-service/service.yaml",
        "services/gateway/service.yaml",
        "services/jaeger/service.yaml",
        "services/judge-api/service.yaml",
        "services/judge-worker/service.yaml",
        "services/minio/service.yaml",
        "services/orchestrator/service.yaml",
        "services/postgresql/service.yaml",
        "services/problem-service/service.yaml",
        "services/redis/service.yaml",
        "services/storage-service/service.yaml",
        "services/user-service/service.yaml",
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
fn checked_in_service_releases_validate() {
    let root = repo_root();
    let expected = [
        "services/auth-service/release.yaml",
        "services/gateway/release.yaml",
        "services/jaeger/release.yaml",
        "services/judge-api/release.yaml",
        "services/judge-worker/release.yaml",
        "services/minio/release.yaml",
        "services/orchestrator/release.yaml",
        "services/postgresql/release.yaml",
        "services/problem-service/release.yaml",
        "services/redis/release.yaml",
        "services/storage-service/release.yaml",
        "services/user-service/release.yaml",
    ];
    let mut actual = relative_files(&root, &root.join("services"))
        .into_iter()
        .filter(|path| path.ends_with("/release.yaml"))
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
        "checked-in Service releases should stay at the formal release.yaml set"
    );

    for path in expected {
        let release = validate_service_release_file(&root, Path::new(path))
            .unwrap_or_else(|err| panic!("{path} should validate: {err}"));
        for migration in &release.migrations {
            assert!(
                migration
                    .path
                    .starts_with(&format!("services/{}/migrations/", release.service_name)),
                "{path} migration {} must stay under its service migrations directory",
                migration.version
            );
            let migration_path = root.join(&migration.path);
            assert!(
                migration_path.is_file(),
                "{path} migration file should exist: {}",
                migration.path
            );
            let bytes = fs::read(&migration_path).expect("read migration for checksum");
            let digest = sha256_hex(&bytes);
            assert_eq!(
                migration.checksum,
                format!("sha256:{digest}"),
                "{path} migration {} checksum must match file content",
                migration.version
            );
        }
    }
}

#[test]
fn service_release_routes_must_cover_service_manifest_routes() {
    let dir = tempdir().unwrap();
    let service_dir = dir.path().join("services/demo-api");
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("service.yaml"),
        r#"
schema_version: 1
id: demo-api
name: Demo API
version: 0.1.0
kind: backend-api
description: Demo service
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
endpoint:
  protocol: http
  default_port: 18080
  health_path: /health
  routes:
    - /api/demo
requires:
  services:
    - redis
  links:
    - id: redis
      protocol: redis
provides:
  routes:
    - /api/demo
source:
  type: local
  ref: services/demo-api
health:
  checks: [http]
  timeout_seconds: 3
  interval_seconds: 10
"#,
    )
    .unwrap();
    fs::write(
        service_dir.join("release.yaml"),
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
backend:
  protocol: http
  port: 18080
  health_path: /health
routes:
  - path: /api/demos/**
    target_type: endpoint-group
    target: demo-api[*]
    permission: public
dependencies:
  - redis
"#,
    )
    .unwrap();

    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-api/release.yaml"))
            .expect_err("release route must cover the service route prefix");
    assert!(err.to_string().contains("release routes must cover"));
}

#[test]
fn service_release_must_match_service_manifest_runtime_contract() {
    let dir = tempdir().unwrap();
    let service_dir = dir.path().join("services/demo-api");
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("service.yaml"),
        r#"
schema_version: 1
id: demo-api
name: Demo API
version: 0.1.0
kind: backend-api
description: Demo service
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
endpoint:
  protocol: http
  default_port: 18080
  health_path: /health
  routes:
    - /api/demo
requires:
  services:
    - redis
provides:
  routes:
    - /api/demo
  storage_buckets:
    - demo-bucket
source:
  type: local
  ref: services/demo-api
ui:
  enabled: true
  routes:
    - /demo
permissions:
  - demo.read
health:
  checks: [http]
  timeout_seconds: 3
  interval_seconds: 10
"#,
    )
    .unwrap();
    fs::write(
        service_dir.join("release.yaml"),
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: cache
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
frontend:
  enabled: true
  route_prefix: /demo
  remote_entry: /assets/demo-api/remoteEntry.js
backend:
  protocol: http
  port: 18080
  health_path: /ready
permissions:
  - demo.read
routes:
  - path: /api/demo/**
    target_type: endpoint-group
    target: demo-api[*]
    permission: public
storage:
  - object_type: demo
    bucket: demo-bucket
dependencies: []
"#,
    )
    .unwrap();

    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-api/release.yaml"))
            .expect_err("release service_type must match service.yaml kind");
    assert!(
        err.to_string()
            .contains("release service_type must match service.yaml kind")
    );

    let release_text = fs::read_to_string(service_dir.join("release.yaml"))
        .unwrap()
        .replace("service_type: cache", "service_type: backend-api");
    fs::write(service_dir.join("release.yaml"), release_text).unwrap();
    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-api/release.yaml"))
            .expect_err("release health_path must match service endpoint health_path");
    assert!(
        err.to_string()
            .contains("release backend health_path must match")
    );

    let release_text = fs::read_to_string(service_dir.join("release.yaml"))
        .unwrap()
        .replace("health_path: /ready", "health_path: /health");
    fs::write(service_dir.join("release.yaml"), release_text).unwrap();
    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-api/release.yaml"))
            .expect_err("release dependencies must cover service requirements");
    assert!(
        err.to_string()
            .contains("release dependencies must cover service.yaml requires.services")
    );
}

#[test]
fn service_release_rejects_legacy_service_set_route_target_type() {
    let release: ServiceReleaseManifest = serde_yaml::from_str(
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
backend:
  protocol: http
  port: 18080
  health_path: /health
routes:
  - path: /api/demo/**
    target_type: service-set
    target: demo-api[*]
    permission: public
"#,
    )
    .expect("release should parse");

    let err =
        validate_service_release(&release).expect_err("service-set is not a formal route target");
    assert!(
        err.to_string()
            .contains("release route target_type is invalid")
    );
}

#[test]
fn service_release_endpoint_group_target_must_be_service_name_star() {
    let release: ServiceReleaseManifest = serde_yaml::from_str(
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
backend:
  protocol: http
  port: 18080
  health_path: /health
routes:
  - path: /api/demo/**
    target_type: endpoint-group
    target: demo-set[*]
    permission: public
"#,
    )
    .expect("release should parse");

    let err = validate_service_release(&release)
        .expect_err("endpoint-group target must be the service-name running endpoint set");
    assert!(
        err.to_string()
            .contains("endpoint-group route target must be service-name[*]")
    );
}

#[test]
fn service_release_endpoint_target_must_match_service_name() {
    let release: ServiceReleaseManifest = serde_yaml::from_str(
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
backend:
  protocol: http
  port: 18080
  health_path: /health
routes:
  - path: /api/demo/**
    target_type: endpoint
    target: 127.0.0.1:18080:other-api
    permission: public
"#,
    )
    .expect("release should parse");

    let err = validate_service_release(&release)
        .expect_err("endpoint route target third segment must match release service_name");
    assert!(
        err.to_string()
            .contains("endpoint route target must match service_name")
    );
}

#[test]
fn service_release_rejects_duplicate_runtime_declarations() {
    let release: ServiceReleaseManifest = serde_yaml::from_str(
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
backend:
  protocol: http
  port: 18080
  health_path: /health
migrations:
  - version: "0001"
    path: services/demo-api/migrations/0001.sql
  - version: "0001"
    path: services/demo-api/migrations/0002.sql
routes:
  - path: /api/demo/**
    method: GET
    target_type: endpoint-group
    target: demo-api[*]
    permission: public
redis:
  - name: ojos:demo:task
    kind: stream
    usage: queue
storage:
  - object_type: demo
    bucket: demo-bucket
"#,
    )
    .expect("release should parse");

    let err =
        validate_service_release(&release).expect_err("release migration versions must be unique");
    assert!(
        err.to_string()
            .contains("duplicate release migration version")
    );

    let mut release = release;
    release.migrations[1].version = "0002".to_string();
    release.routes.push(release.routes[0].clone());
    let err = validate_service_release(&release).expect_err("release routes must be unique");
    assert!(err.to_string().contains("duplicate release route"));

    release.routes.pop();
    release.routes[0].method = "TRACE".to_string();
    let err = validate_service_release(&release).expect_err("release route method is fixed");
    assert!(err.to_string().contains("release route method is invalid"));

    release.routes[0].method = "ANY".to_string();
    release.redis.push(release.redis[0].clone());
    let err = validate_service_release(&release).expect_err("release redis names must be unique");
    assert!(err.to_string().contains("duplicate release redis resource"));

    release.redis.pop();
    release.storage.push(release.storage[0].clone());
    let err =
        validate_service_release(&release).expect_err("release storage resources must be unique");
    assert!(
        err.to_string()
            .contains("duplicate release storage resource")
    );

    release.storage.pop();
    release.storage[0].path_prefix = "../bad".to_string();
    let err = validate_service_release(&release).expect_err("storage path_prefix is scoped");
    assert!(
        err.to_string()
            .contains("release storage path_prefix is invalid")
    );
}

#[test]
fn service_release_api_surface_validation_rules_are_enforced() {
    let mut release = valid_release_for_service(&valid_service());
    release.apis = vec![ReleaseApiSurfaceDecl {
        api_id: "demo.read".to_string(),
        protocol: "http".to_string(),
        port_name: "http".to_string(),
        path_prefix: "/api/demo".to_string(),
        methods: vec!["GET".to_string()],
        visibility: "descendants".to_string(),
        auth_mode: "service".to_string(),
        permission: "demo.read".to_string(),
        stability: "stable".to_string(),
        version: "v1".to_string(),
        ..Default::default()
    }];
    validate_service_release(&release).expect("valid release with apis passes");

    let mut duplicate = release.clone();
    duplicate.apis.push(duplicate.apis[0].clone());
    let err = validate_service_release(&duplicate).expect_err("duplicate api_id should fail");
    assert!(err.to_string().contains("duplicate release api_id"));

    let mut missing_port = release.clone();
    missing_port.apis[0].port_name = "admin".to_string();
    let err = validate_service_release(&missing_port).expect_err("missing port should fail");
    assert!(
        err.to_string()
            .contains("release api port_name does not exist")
    );

    let mut missing_permission = release.clone();
    missing_permission.apis[0].permission = "demo.write".to_string();
    let err = validate_service_release(&missing_permission)
        .expect_err("undeclared permission should fail");
    assert!(
        err.to_string()
            .contains("release api permission must be declared")
    );

    let mut invalid_visibility = release;
    invalid_visibility.apis[0].visibility = "siblings".to_string();
    let err =
        validate_service_release(&invalid_visibility).expect_err("invalid visibility should fail");
    assert!(
        err.to_string()
            .contains("release api visibility is invalid")
    );
}

#[test]
fn service_release_must_cover_service_permissions_frontend_and_storage() {
    let dir = tempdir().unwrap();
    let service_dir = dir.path().join("services/demo-api");
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("service.yaml"),
        r#"
schema_version: 1
id: demo-api
name: Demo API
version: 0.1.0
kind: backend-api
description: Demo service
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
endpoint:
  protocol: http
  default_port: 18080
  health_path: /health
  routes: []
requires: {}
provides:
  storage_buckets:
    - demo-bucket
source:
  type: local
  ref: services/demo-api
ui:
  enabled: true
  routes:
    - /demo
permissions:
  - demo.read
health:
  checks: [http]
  timeout_seconds: 3
  interval_seconds: 10
"#,
    )
    .unwrap();
    fs::write(
        service_dir.join("release.yaml"),
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
frontend:
  enabled: false
backend:
  protocol: http
  port: 18080
  health_path: /health
permissions: []
routes: []
storage: []
"#,
    )
    .unwrap();

    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-api/release.yaml"))
            .expect_err("release must cover service permissions and frontend declarations");
    assert!(
        err.to_string()
            .contains("release permissions must cover service.yaml permissions")
    );
}

#[test]
fn service_release_must_cover_service_secrets() {
    let dir = tempdir().unwrap();
    let service_dir = dir.path().join("services/demo-api");
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("service.yaml"),
        r#"
schema_version: 1
id: demo-api
name: Demo API
version: 0.1.0
kind: backend-api
description: Demo service
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
endpoint:
  protocol: http
  default_port: 18080
  health_path: /health
requires:
  secrets:
    - demo-token
provides:
  routes: []
  storage_buckets: []
source:
  type: local
  ref: services/demo-api
ui:
  enabled: false
security:
  required_secrets:
    - demo-token
health:
  checks: [http]
  timeout_seconds: 3
  interval_seconds: 10
"#,
    )
    .unwrap();
    fs::write(
        service_dir.join("release.yaml"),
        r#"
schema_version: 1
service_name: demo-api
version: 0.1.0
description: Demo release
service_type: backend-api
source:
  kind: local
  url: local://services/demo-api
runtime:
  kind: image
backend:
  protocol: http
  port: 18080
  health_path: /health
permissions: []
routes: []
storage: []
dependencies: []
secrets: []
"#,
    )
    .unwrap();

    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-api/release.yaml"))
            .expect_err("release must cover service secrets");
    assert!(
        err.to_string()
            .contains("release secrets must cover service.yaml secrets")
    );
}

#[test]
fn service_release_must_cover_ui_permissions_and_queue_redis() {
    let dir = tempdir().unwrap();
    let service_dir = dir.path().join("services/demo-worker");
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("service.yaml"),
        r#"
schema_version: 1
id: demo-worker
name: Demo Worker
version: 0.1.0
kind: backend-worker
description: Demo worker
runtime:
  mode: container
  root_allowed: true
  non_root_allowed: false
endpoint:
  protocol: http
  default_port: 18081
  health_path: /health
requires:
  services:
    - redis
  links:
    - id: redis
      protocol: redis
  queue:
    - redis
  secrets: []
provides:
  routes: []
  storage_buckets: []
source:
  type: local
  ref: services/demo-worker
ui:
  enabled: true
  routes:
    - /worker
  permissions:
    - demo.worker.view
permissions:
  - demo.worker.run
health:
  checks: [http]
  timeout_seconds: 3
  interval_seconds: 10
"#,
    )
    .unwrap();
    fs::write(
        service_dir.join("release.yaml"),
        r#"
schema_version: 1
service_name: demo-worker
version: 0.1.0
description: Demo worker release
service_type: backend-worker
source:
  kind: local
  url: local://services/demo-worker
runtime:
  kind: image
frontend:
  enabled: true
  route_prefix: /worker
  remote_entry: /assets/demo-worker/remoteEntry.js
backend:
  protocol: http
  port: 18081
  health_path: /health
permissions:
  - demo.worker.run
routes: []
redis: []
storage: []
dependencies:
  - redis
secrets: []
"#,
    )
    .unwrap();

    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-worker/release.yaml"))
            .expect_err("release must cover UI permissions before install");
    assert!(
        err.to_string()
            .contains("release permissions must cover service.yaml ui permissions")
    );

    let release_text = fs::read_to_string(service_dir.join("release.yaml"))
        .unwrap()
        .replace(
            "permissions:\n  - demo.worker.run",
            "permissions:\n  - demo.worker.run\n  - demo.worker.view",
        );
    fs::write(service_dir.join("release.yaml"), release_text).unwrap();
    let err =
        validate_service_release_file(dir.path(), Path::new("services/demo-worker/release.yaml"))
            .expect_err("release must cover queue redis resources before install");
    assert!(
        err.to_string()
            .contains("release redis must cover service.yaml requires.queue")
    );
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
fn service_manifest_rejects_declared_endpoint_names() {
    let mut manifest = valid_service();
    manifest.provides.endpoints.push("http".to_string());
    let err = validate_service_manifest(&manifest)
        .expect_err("runtime endpoints must be derived from deployment identity");
    assert!(err.to_string().contains("ip:port:service-name"));
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
fn endpoint_requires_ip_port_service_name() {
    validate_endpoint_id("192.168.1.10:8080:gateway").expect("endpoint");
    assert!(validate_endpoint_id("192.168.1.10:0:gateway").is_err());
    assert!(validate_endpoint_id("192.168.1.10:8080:Gateway").is_err());
    assert!(validate_endpoint_id("localhost:8080:gateway").is_err());
    assert!(validate_endpoint_id("192.168.1.10").is_err());
    assert!(validate_endpoint_id("192.168.1.10:8080").is_err());
    assert!(validate_endpoint_id("192.168.1.10:8080:gateway:extra").is_err());
    let parsed = parse_endpoint_id("192.168.1.10:8080:gateway").expect("endpoint");
    assert_eq!(parsed.host, "192.168.1.10");
    assert_eq!(parsed.port, "8080");
    assert_eq!(parsed.service_name, "gateway");
    assert_eq!(
        endpoint_socket_addr("192.168.1.10:8080:gateway").expect("socket addr"),
        "192.168.1.10:8080"
    );
}

#[test]
fn endpoint_service_name_must_match_endpoint_identity() {
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
        service_id: "auth-service".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: String::new(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let err =
        validate_endpoint(&endpoint).expect_err("third segment must match endpoint service_id");
    assert!(
        err.to_string()
            .contains("service name must match service_id")
    );
}

#[test]
fn set_validate_and_expand() {
    let root = repo_root();
    let set = validate_deployment_template_file(&root, Path::new("sets/distributed-oj.yaml"))
        .expect("distributed oj set");
    let expanded = preview_deployment_template(&set);
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
        "only the five local deployment templates should remain"
    );

    for entry in fs::read_dir(root.join("sets")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|value| value.to_str()) != Some("yaml") {
            continue;
        }
        let rel = Path::new("sets").join(entry.file_name());
        let set = validate_deployment_template_file(&root, &rel)
            .unwrap_or_else(|err| panic!("{} should validate: {err}", rel.display()));
        validate_deployment_template_references(&root, &set)
            .unwrap_or_else(|err| panic!("{} references should validate: {err}", rel.display()));
    }
}

#[test]
fn set_rejects_missing_service_references() {
    let root = repo_root();
    let mut set =
        validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
    set.services
        .push(DeploymentTemplateService::Id("missing-service".to_string()));
    assert!(validate_deployment_template_references(&root, &set).is_err());
}

#[test]
fn set_references_must_cover_service_required_links() {
    let root = repo_root();
    let mut set =
        validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
    set.default_links
        .retain(|link| !(link.from == "gateway" && link.to == "problem-service"));

    let err = validate_deployment_template_references(&root, &set)
        .expect_err("set should require default links for in-set service requirements");
    assert!(err.to_string().contains("default_links must cover"));

    let mut worker_set =
        validate_deployment_template_file(&root, Path::new("sets/judge-worker-node.yaml")).unwrap();
    worker_set.policies["network"]["required_external_links"] = serde_json::json!([]);
    let err = validate_deployment_template_references(&root, &worker_set)
        .expect_err("external service requirements should be declared explicitly");
    assert!(
        err.to_string()
            .contains("policies.network.required_external_links")
    );
}

#[test]
fn release_install_operation_uses_operation_model() {
    let manifest = valid_service();
    let operation =
        release_install_operation("op-service-install", &manifest, &[]).expect("install operation");
    assert_eq!(operation.status, OperationStatus::Planned);
    assert_eq!(operation.action, "release.install");
    assert_eq!(operation.target_type, "ServiceRelease");
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
    assert!(
        operation
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|steps| steps.iter().any(|item| item
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("allocate_endpoint")
                && item.get("target").and_then(serde_json::Value::as_str)
                    == Some("127.0.0.1:18080:demo-api"))),
        "release.install plan should allocate a concrete ip:port:service-name endpoint"
    );
    assert!(
        operation
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|steps| steps.iter().any(|item| item
                .get("action")
                .and_then(serde_json::Value::as_str)
                == Some("create_host_service")
                && item.get("target").and_then(serde_json::Value::as_str)
                    == Some("127.0.0.1:demo-api"))),
        "release.install plan should produce host_ip + service-name deployment state"
    );
}

#[test]
fn release_install_planner_uses_release_manifest() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/judge-api/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/judge-api/release.yaml")).unwrap();
    let request = ActionRequest::new(
        "op-release-aware-install",
        "release.install",
        [
            ("service_id".to_string(), "judge-api".to_string()),
            ("host_ip".to_string(), "10.77.0.2".to_string()),
        ]
        .into_iter()
        .collect(),
    );

    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release-aware install operation");

    assert_eq!(
        operation
            .request
            .get("release_manifest")
            .and_then(|value| value.get("service_name"))
            .and_then(serde_json::Value::as_str),
        Some("judge-api")
    );
    let steps = operation
        .plan
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .expect("steps");
    for expected in [
        "fetch_or_load_release_package",
        "validate_service_release",
        "select_host",
        "create_host_service",
        "allocate_endpoint",
        "register_permissions",
        "sync_auth_permissions",
        "register_gateway_routes",
        "publish_gateway_routes",
        "register_frontend_entry",
        "register_service_migrations",
        "run_service_migrations",
        "register_redis_resources",
        "provision_redis_resources",
        "register_storage_resources",
        "provision_storage_resources",
        "render_service_config",
        "dispatch_to_node_or_standalone",
        "start_service",
        "health_probe",
        "mark_running_state",
    ] {
        assert!(
            steps.iter().any(
                |step| step.get("action").and_then(serde_json::Value::as_str) == Some(expected)
            ),
            "missing release-driven step {expected}"
        );
    }
    assert_eq!(
        operation
            .request
            .get("endpoint")
            .and_then(serde_json::Value::as_str),
        Some("10.77.0.2:8082:judge-api")
    );
}

#[test]
fn release_install_planner_uses_package_source_override() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/judge-api/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/judge-api/release.yaml")).unwrap();
    let request = ActionRequest::new(
        "op-release-package-install",
        "release.install",
        [
            ("service_id".to_string(), "judge-api".to_string()),
            ("host_ip".to_string(), "10.77.0.2".to_string()),
            (
                "release_url".to_string(),
                "D:/tmp/judge-api-release.zip".to_string(),
            ),
            ("release_checksum".to_string(), "sha256:abc123".to_string()),
        ]
        .into_iter()
        .collect(),
    );

    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release package install operation");

    assert_eq!(
        operation
            .request
            .get("release_manifest")
            .and_then(|value| value.get("source"))
            .and_then(|value| value.get("url"))
            .and_then(serde_json::Value::as_str),
        Some("D:/tmp/judge-api-release.zip")
    );
    assert_eq!(
        operation
            .request
            .get("release_checksum")
            .and_then(serde_json::Value::as_str),
        Some("sha256:abc123")
    );
}

#[test]
fn release_aware_planner_requires_release_manifest_when_releases_are_loaded() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/judge-api/release.yaml")).unwrap();
    let request = ActionRequest::new(
        "op-release-aware-install-missing",
        "release.install",
        [("service_id".to_string(), "gateway".to_string())]
            .into_iter()
            .collect(),
    );

    let err = plan_action_request_with_releases(&request, &[service], &[release], &[], &[], None)
        .expect_err("release-aware planner should require the matching release");
    assert!(
        err.to_string()
            .contains("missing ServiceRelease manifest gateway")
    );
}

#[test]
fn release_install_executor_records_release_resources() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let mut release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    release.runtime = ReleaseRuntimeDecl {
        kind: "image".to_string(),
        image: String::new(),
        binary: String::new(),
        system_service: String::new(),
        command: String::new(),
        args: Vec::new(),
        working_dir: String::new(),
        env: BTreeMap::new(),
    };
    let request = ActionRequest::new(
        "op-release-apply-with-resources",
        "release.install",
        [("service_id".to_string(), "gateway".to_string())]
            .into_iter()
            .collect(),
    );
    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release-aware operation");
    let confirmed = confirm_operation(&operation).expect("confirm release install");

    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(confirmed).expect("put operation");
    let applied = OperationExecutor::new(&mut store)
        .apply("op-release-apply-with-resources")
        .expect("apply release install");

    let changed = applied
        .result
        .get("changed_objects")
        .and_then(serde_json::Value::as_array)
        .expect("changed objects");
    for expected_type in [
        "ServiceRelease",
        "HostService",
        "Endpoint",
        "Permission",
        "Route",
        "Frontend",
        "RenderedConfig",
        "Service",
    ] {
        assert!(
            changed
                .iter()
                .any(|item| item.get("type").and_then(serde_json::Value::as_str)
                    == Some(expected_type)),
            "missing changed object type {expected_type}"
        );
    }
    assert_eq!(
        store
            .get_service("gateway")
            .expect("get service")
            .expect("gateway stored")
            .id,
        "gateway"
    );
    let host_service = store
        .get_host_service("127.0.0.1", "gateway")
        .expect("get host service")
        .expect("gateway host service stored");
    assert_eq!(host_service.version, release.version);
    assert_eq!(host_service.status, "planned");
    assert_eq!(
        host_service
            .config
            .get("external_steps")
            .and_then(|steps| steps.get("service_start"))
            .and_then(serde_json::Value::as_str),
        Some("PLANNED")
    );
    assert_eq!(
        host_service
            .config
            .get("external_steps")
            .and_then(|steps| steps.get("node_dispatch"))
            .and_then(serde_json::Value::as_str),
        Some("planned")
    );
    let runtime_pipeline = applied
        .result
        .get("runtime_pipeline")
        .expect("release.install result should expose runtime pipeline status");
    assert_eq!(
        runtime_pipeline
            .get("host_service")
            .and_then(serde_json::Value::as_str),
        Some("created")
    );
    assert_eq!(
        runtime_pipeline
            .get("endpoint_allocation")
            .and_then(serde_json::Value::as_str),
        Some("created")
    );
    assert_eq!(
        runtime_pipeline
            .get("endpoint")
            .and_then(serde_json::Value::as_str),
        Some("127.0.0.1:8080:gateway")
    );
    assert_eq!(
        runtime_pipeline
            .get("release_package")
            .and_then(|package| package.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("planned")
    );
    assert_eq!(
        runtime_pipeline
            .get("node_dispatch")
            .and_then(serde_json::Value::as_str),
        Some("planned")
    );
    assert_eq!(
        runtime_pipeline
            .get("node_dispatch_result")
            .and_then(|node| node.get("accepted"))
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        runtime_pipeline
            .get("service_driver")
            .and_then(|driver| driver.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("PLANNED")
    );
    assert_eq!(
        runtime_pipeline
            .get("health")
            .and_then(|health| health.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("deferred")
    );
    assert_eq!(
        runtime_pipeline
            .get("gateway_route_update")
            .and_then(serde_json::Value::as_str),
        Some("planned")
    );
    assert_eq!(
        store
            .get_endpoint("127.0.0.1:8080:gateway")
            .expect("get endpoint")
            .expect("gateway endpoint stored")
            .service_id,
        "gateway"
    );
    let stored_release = store
        .get_service_release("gateway", &release.version)
        .expect("get stored release")
        .expect("gateway release stored");
    assert_eq!(stored_release.service_name, "gateway");
    assert_eq!(
        stored_release
            .manifest
            .get("service_name")
            .and_then(serde_json::Value::as_str),
        Some("gateway")
    );
    let routes = store.service_routes();
    assert!(
        routes
            .iter()
            .any(|route| route.target_service_name == "gateway"
                && route.target_type == "endpoint-group"
                && route
                    .target_selector
                    .get("group")
                    .and_then(serde_json::Value::as_str)
                    == Some("gateway[*]")),
        "release routes should be persisted in formal route registry"
    );
    assert_eq!(
        store.service_migration_records().len(),
        release.migrations.len()
    );
    assert!(
        store
            .service_migration_records()
            .iter()
            .all(|record| record.status == "registered"),
        "release.install must not mark migrations applied until a real runner succeeds"
    );
    assert_eq!(
        store.service_permission_records().len(),
        release.permissions.len()
    );
    assert!(
        store
            .service_permission_records()
            .iter()
            .any(|permission| permission.service_name == "gateway"
                && permission.permission_key == "gateway.read")
    );
    assert_eq!(store.service_frontend_entries()[0].service_name, "gateway");
    assert_eq!(
        store.service_frontend_entries()[0].route_prefix,
        release.frontend.route_prefix
    );
    assert_eq!(store.service_redis_resources().len(), release.redis.len());
    assert_eq!(
        store.service_storage_resources().len(),
        release.storage.len()
    );
    assert_eq!(store.rendered_service_configs()[0].service_name, "gateway");
    assert_eq!(
        store.rendered_service_configs()[0]
            .config
            .get("backend")
            .and_then(|backend| backend.get("protocol"))
            .and_then(serde_json::Value::as_str),
        Some("http")
    );
    let view = load_orchestrator_view_from_store(load_shared_schemas(&root).unwrap(), &store)
        .expect("store-backed view");
    assert!(
        view.release_registry.iter().any(|row| {
            row.service_name == "gateway" && row.record_type == "route" && row.source == "store"
        }),
        "store-backed view should expose release.install registry rows"
    );
    let logs = store.operation_logs("op-release-apply-with-resources");
    assert!(logs.iter().any(|log| log.step_id == "release:gateway"));
    assert!(logs.iter().any(|log| {
        log.step_id == "release-package:gateway"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("planned")
    }));
    assert!(
        logs.iter()
            .any(|log| log.step_id == "install-pipeline:gateway")
    );
    assert!(logs.iter().any(|log| {
        log.data
            .get("frontend_enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    }));
    assert!(logs.iter().any(|log| {
        log.data
            .get("node_dispatch")
            .and_then(serde_json::Value::as_str)
            == Some("planned")
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "gateway_reload:gateway"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("planned")
            && log
                .data
                .get("reloaded")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:gateway"
            && log
                .data
                .get("gateway_route_update")
                .and_then(serde_json::Value::as_str)
                == Some("planned")
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "node-dispatch:gateway"
            && log
                .data
                .get("accepted")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
    }));
    assert!(logs.iter().any(|log| {
        log.data
            .get("health")
            .and_then(|health| health.get("status"))
            .and_then(serde_json::Value::as_str)
            == Some("deferred")
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "driver:release.install"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("PLANNED")
    }));
}

#[test]
fn release_install_runtime_pipeline_result_covers_declared_resources() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/judge-api/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/judge-api/release.yaml")).unwrap();
    assert!(
        !release.migrations.is_empty()
            && !release.permissions.is_empty()
            && !release.routes.is_empty()
            && !release.redis.is_empty()
            && !release.storage.is_empty()
            && release.frontend.enabled,
        "judge-api release should cover the full runtime pipeline fixture"
    );
    let operation = release_install_operation_with_release(
        "op-release-judge-api-runtime-pipeline",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("release install operation");

    let mut store = MemoryOrchestratorStore::new();
    seed_storage_identity_api_surfaces(&mut store);
    store.put_operation(operation).expect("put operation");
    let applied = OperationExecutor::new(&mut store)
        .apply("op-release-judge-api-runtime-pipeline")
        .expect("apply release install");

    let runtime_pipeline = applied
        .result
        .get("runtime_pipeline")
        .expect("runtime pipeline result");
    assert_eq!(
        runtime_pipeline
            .get("endpoint")
            .and_then(serde_json::Value::as_str),
        Some("127.0.0.1:8082:judge-api")
    );
    assert_eq!(
        runtime_pipeline
            .get("migrations")
            .and_then(|migrations| migrations.get("count"))
            .and_then(serde_json::Value::as_u64),
        Some(release.migrations.len() as u64)
    );
    assert_eq!(
        runtime_pipeline
            .get("migrations")
            .and_then(|migrations| migrations.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("deferred")
    );
    assert_eq!(
        runtime_pipeline
            .get("permission_registration")
            .and_then(serde_json::Value::as_str),
        Some("skipped")
    );
    assert_eq!(
        runtime_pipeline
            .get("auth_permission_registration")
            .and_then(|registration| registration.get("registered"))
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
    assert_eq!(
        runtime_pipeline
            .get("gateway_route_update")
            .and_then(serde_json::Value::as_str),
        Some("planned")
    );
    assert_eq!(
        runtime_pipeline
            .get("gateway_route_publish")
            .and_then(|publish| publish.get("route_count"))
            .and_then(serde_json::Value::as_u64),
        Some(release.routes.len() as u64)
    );
    assert_eq!(
        runtime_pipeline
            .get("frontend_registration")
            .and_then(serde_json::Value::as_str),
        Some("registry-only")
    );
    assert_eq!(
        runtime_pipeline
            .get("redis_resources")
            .and_then(serde_json::Value::as_str),
        Some("skipped")
    );
    assert_eq!(
        runtime_pipeline
            .get("redis_provision")
            .and_then(|provision| provision.get("provisioned"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(
        runtime_pipeline
            .get("storage_resources")
            .and_then(serde_json::Value::as_str),
        Some("skipped")
    );
    assert_eq!(
        runtime_pipeline
            .get("storage_provision")
            .and_then(|provision| provision.get("provisioned"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(0)
    );

    assert_eq!(
        store.service_migration_records().len(),
        release.migrations.len()
    );
    assert_eq!(
        store.service_permission_records().len(),
        release.permissions.len()
    );
    assert_eq!(store.service_routes().len(), release.routes.len());
    assert_eq!(store.service_redis_resources().len(), release.redis.len());
    assert_eq!(
        store.service_storage_resources().len(),
        release.storage.len()
    );
}

#[test]
fn release_install_runtime_pipeline_result_reports_configured_runtime_success() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/judge-api/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/judge-api/release.yaml")).unwrap();
    assert!(
        !release.migrations.is_empty()
            && !release.permissions.is_empty()
            && !release.routes.is_empty()
            && !release.redis.is_empty()
            && !release.storage.is_empty()
            && release.frontend.enabled,
        "judge-api release should cover the configured runtime pipeline fixture"
    );
    let operation = release_install_operation_with_release(
        "op-release-judge-api-runtime-success",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("release install operation");

    let auth_calls = Arc::new(Mutex::new(Vec::new()));
    let redis_calls = Arc::new(Mutex::new(Vec::new()));
    let storage_calls = Arc::new(Mutex::new(Vec::new()));
    let migration_calls = Arc::new(Mutex::new(Vec::new()));
    let gateway_calls = Arc::new(Mutex::new(Vec::new()));
    let node_calls = Arc::new(Mutex::new(Vec::new()));
    let migration_result = MigrationExecutionResult {
        status: "applied".to_string(),
        message: "recorded by configured runtime pipeline test".to_string(),
        runner: "recording".to_string(),
        dry_run: false,
        executed: release
            .migrations
            .iter()
            .map(|migration| MigrationExecutionRecord {
                migration_version: migration.version.clone(),
                path: migration.path.clone(),
                checksum: migration.checksum.clone(),
                status: "applied".to_string(),
                applied_at: "applied".to_string(),
                message: "applied by configured runtime pipeline test".to_string(),
            })
            .collect(),
    };
    let publisher = RecordingGatewayRoutePublisher {
        calls: Arc::clone(&gateway_calls),
        result: GatewayRoutePublishResult {
            status: "published".to_string(),
            message: "recorded by configured runtime pipeline test".to_string(),
            endpoint: "http://gateway.test".to_string(),
            route_count: 0,
            reloaded: true,
        },
    };
    let dispatcher = RecordingNodeServiceDispatcher {
        calls: Arc::clone(&node_calls),
        result: NodeServiceDispatchResult {
            status: "ACCEPTED".to_string(),
            message: "recorded by configured runtime pipeline test".to_string(),
            endpoint: "127.0.0.1:8082:judge-api".to_string(),
            accepted: true,
        },
    };

    let mut store = MemoryOrchestratorStore::new();
    seed_storage_identity_api_surfaces(&mut store);
    store.put_operation(operation).expect("put operation");
    let applied =
        OperationExecutor::with_runtime_provisioners_release_loader_gateway_publisher_and_node_dispatcher(
            &mut store,
            StaticEndpointProbe,
            RecordingAuthPermissionRegistrar {
                calls: Arc::clone(&auth_calls),
            },
            RecordingRedisResourceProvisioner {
                calls: Arc::clone(&redis_calls),
            },
            RecordingStorageResourceProvisioner {
                calls: Arc::clone(&storage_calls),
            },
            RecordingMigrationRunner {
                calls: Arc::clone(&migration_calls),
                result: migration_result,
            },
            DeferredReleasePackageLoader,
            publisher,
            dispatcher,
        )
        .apply("op-release-judge-api-runtime-success")
        .expect("apply release install with configured runtime providers");

    let runtime_pipeline = applied
        .result
        .get("runtime_pipeline")
        .expect("runtime pipeline result");
    assert_eq!(
        runtime_pipeline
            .get("endpoint")
            .and_then(serde_json::Value::as_str),
        Some("127.0.0.1:8082:judge-api")
    );
    assert_eq!(
        runtime_pipeline
            .get("migrations")
            .and_then(|migrations| migrations.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("applied")
    );
    assert_eq!(
        runtime_pipeline
            .get("permission_registration")
            .and_then(serde_json::Value::as_str),
        Some("registered")
    );
    assert_eq!(
        runtime_pipeline
            .get("auth_permission_registration")
            .and_then(|registration| registration.get("registered"))
            .and_then(serde_json::Value::as_u64),
        Some(release.permissions.len() as u64)
    );
    assert_eq!(
        runtime_pipeline
            .get("gateway_route_update")
            .and_then(serde_json::Value::as_str),
        Some("published")
    );
    assert_eq!(
        runtime_pipeline
            .get("gateway_route_publish")
            .and_then(|publish| publish.get("reloaded"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        runtime_pipeline
            .get("redis_resources")
            .and_then(serde_json::Value::as_str),
        Some("created")
    );
    assert_eq!(
        runtime_pipeline
            .get("redis_provision")
            .and_then(|provision| provision.get("provisioned"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(release.redis.len())
    );
    assert_eq!(
        runtime_pipeline
            .get("storage_resources")
            .and_then(serde_json::Value::as_str),
        Some("ensured")
    );
    assert_eq!(
        runtime_pipeline
            .get("storage_provision")
            .and_then(|provision| provision.get("provisioned"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(release.storage.len())
    );
    assert_eq!(
        runtime_pipeline
            .get("node_dispatch")
            .and_then(serde_json::Value::as_str),
        Some("ACCEPTED")
    );
    assert_eq!(
        runtime_pipeline
            .get("node_dispatch_result")
            .and_then(|node| node.get("accepted"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        runtime_pipeline
            .get("service_driver")
            .and_then(|driver| driver.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("PLANNED")
    );
    assert_eq!(
        runtime_pipeline
            .get("health")
            .and_then(|health| health.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("deferred")
    );

    assert_eq!(auth_calls.lock().expect("auth calls").len(), 1);
    assert_eq!(
        auth_calls.lock().expect("auth calls")[0].permissions.len(),
        release.permissions.len()
    );
    assert_eq!(redis_calls.lock().expect("redis calls").len(), 1);
    assert_eq!(
        redis_calls.lock().expect("redis calls")[0].resources.len(),
        release.redis.len()
    );
    assert_eq!(storage_calls.lock().expect("storage calls").len(), 1);
    assert_eq!(
        storage_calls.lock().expect("storage calls")[0]
            .resources
            .len(),
        release.storage.len()
    );
    assert_eq!(migration_calls.lock().expect("migration calls").len(), 1);
    assert_eq!(
        migration_calls.lock().expect("migration calls")[0]
            .migrations
            .len(),
        release.migrations.len()
    );
    assert_eq!(gateway_calls.lock().expect("gateway calls").len(), 1);
    assert_eq!(
        gateway_calls.lock().expect("gateway calls")[0].routes.len(),
        release.routes.len()
    );
    let node_calls = node_calls.lock().expect("node calls");
    assert_eq!(node_calls.len(), 1);
    assert_eq!(node_calls[0].endpoint.endpoint, "127.0.0.1:8082:judge-api");
    assert_eq!(node_calls[0].endpoint.service_id, "judge-api");
    assert_eq!(node_calls[0].host_service.host_ip, "127.0.0.1");
    assert_eq!(node_calls[0].host_service.service_name, "judge-api");
    drop(node_calls);

    assert!(
        store
            .service_migration_records()
            .iter()
            .all(|record| record.status == "applied")
    );
    assert_eq!(
        store
            .get_host_service("127.0.0.1", "judge-api")
            .expect("get host service")
            .expect("host service")
            .status,
        "planned"
    );
    assert!(
        store
            .get_endpoint("127.0.0.1:8082:judge-api")
            .expect("get endpoint")
            .is_some()
    );
}

#[test]
fn release_install_runtime_pipeline_installs_minimal_oj_stack_in_one_store() {
    let root = repo_root();
    let service_paths = [
        ("auth-service", "services/auth-service/service.yaml"),
        ("storage-service", "services/storage-service/service.yaml"),
        ("judge-api", "services/judge-api/service.yaml"),
        ("judge-worker", "services/judge-worker/service.yaml"),
        ("problem-service", "services/problem-service/service.yaml"),
        ("user-service", "services/user-service/service.yaml"),
        ("gateway", "services/gateway/service.yaml"),
    ];
    let services = service_paths
        .iter()
        .map(|(service_name, path)| {
            (
                *service_name,
                validate_service_manifest_file(&root, Path::new(path))
                    .expect("minimal stack service manifest"),
            )
        })
        .collect::<Vec<_>>();
    let releases = service_paths
        .iter()
        .map(|(service_name, path)| {
            (
                *service_name,
                validate_service_release_file(
                    &root,
                    Path::new(path).with_file_name("release.yaml").as_path(),
                )
                .expect("minimal stack release manifest"),
            )
        })
        .collect::<Vec<_>>();
    let total_permissions = releases
        .iter()
        .map(|(_, release)| release.permissions.len())
        .sum::<usize>();
    let total_routes = releases
        .iter()
        .map(|(_, release)| release.routes.len())
        .sum::<usize>();
    let total_api_surfaces = releases
        .iter()
        .map(|(_, release)| release.apis.len())
        .sum::<usize>();
    let total_frontend = releases
        .iter()
        .filter(|(_, release)| release.frontend.enabled)
        .count();
    let total_migrations = releases
        .iter()
        .map(|(_, release)| release.migrations.len())
        .sum::<usize>();
    let total_redis = releases
        .iter()
        .map(|(_, release)| release.redis.len())
        .sum::<usize>();
    let total_storage = releases
        .iter()
        .map(|(_, release)| release.storage.len())
        .sum::<usize>();

    assert!(total_permissions > 0);
    assert!(total_routes > 0);
    assert!(total_api_surfaces > 0);
    assert!(total_frontend > 0);
    assert!(total_migrations > 0);
    assert!(total_redis > 0);
    assert!(total_storage > 0);
    assert!(
        releases
            .iter()
            .any(|(service_name, release)| *service_name == "judge-worker"
                && release
                    .redis
                    .iter()
                    .any(|redis| redis.kind == "consumer-group")),
        "judge-worker release must declare the consumer group side of the judge queue"
    );

    let auth_calls = Arc::new(Mutex::new(Vec::new()));
    let redis_calls = Arc::new(Mutex::new(Vec::new()));
    let storage_calls = Arc::new(Mutex::new(Vec::new()));
    let migration_calls = Arc::new(Mutex::new(Vec::new()));
    let gateway_calls = Arc::new(Mutex::new(Vec::new()));
    let node_calls = Arc::new(Mutex::new(Vec::new()));
    let mut store = MemoryOrchestratorStore::new();
    let host_ip = "127.0.0.1";

    for (index, (service_name, service)) in services.iter().enumerate() {
        let release = releases
            .iter()
            .find(|(name, _)| name == service_name)
            .map(|(_, release)| release)
            .expect("release for service");
        let operation_id = format!("op-stack-install-{service_name}");
        let operation = release_install_operation_with_release(
            &operation_id,
            service,
            Some(release),
            &[],
            host_ip,
            None,
            serde_json::json!({}),
        )
        .and_then(|operation| confirm_operation(&operation))
        .expect("release install operation");
        let endpoint = format!("{host_ip}:{}:{service_name}", service.endpoint.default_port);
        let migration_result = MigrationExecutionResult {
            status: if release.migrations.is_empty() {
                "none".to_string()
            } else {
                "applied".to_string()
            },
            message: format!("recorded stack migration runner for {service_name}"),
            runner: "recording".to_string(),
            dry_run: false,
            executed: release
                .migrations
                .iter()
                .map(|migration| MigrationExecutionRecord {
                    migration_version: migration.version.clone(),
                    path: migration.path.clone(),
                    checksum: migration.checksum.clone(),
                    status: "applied".to_string(),
                    applied_at: "applied".to_string(),
                    message: format!("applied by stack runtime pipeline test for {service_name}"),
                })
                .collect(),
        };

        store.put_operation(operation).expect("put stack operation");
        let applied =
            OperationExecutor::with_runtime_provisioners_release_loader_gateway_publisher_and_node_dispatcher(
                &mut store,
                StaticEndpointProbe,
                RecordingAuthPermissionRegistrar {
                    calls: Arc::clone(&auth_calls),
                },
                RecordingRedisResourceProvisioner {
                    calls: Arc::clone(&redis_calls),
                },
                RecordingStorageResourceProvisioner {
                    calls: Arc::clone(&storage_calls),
                },
                RecordingMigrationRunner {
                    calls: Arc::clone(&migration_calls),
                    result: migration_result,
                },
                DeferredReleasePackageLoader,
                RecordingGatewayRoutePublisher {
                    calls: Arc::clone(&gateway_calls),
                    result: GatewayRoutePublishResult {
                        status: "published".to_string(),
                        message: format!("recorded stack route publish for {service_name}"),
                        endpoint: "http://gateway.test".to_string(),
                        route_count: 0,
                        reloaded: true,
                    },
                },
                RecordingNodeServiceDispatcher {
                    calls: Arc::clone(&node_calls),
                    result: NodeServiceDispatchResult {
                        status: "ACCEPTED".to_string(),
                        message: format!("recorded stack node dispatch for {service_name}"),
                        endpoint: endpoint.clone(),
                        accepted: true,
                    },
                },
            )
            .apply(&operation_id)
            .expect("apply stack release install");

        let runtime_pipeline = applied
            .result
            .get("runtime_pipeline")
            .expect("runtime pipeline result");
        assert_eq!(
            runtime_pipeline
                .get("endpoint")
                .and_then(serde_json::Value::as_str),
            Some(endpoint.as_str())
        );
        assert_eq!(
            runtime_pipeline
                .get("node_dispatch")
                .and_then(serde_json::Value::as_str),
            Some("ACCEPTED")
        );
        assert_eq!(
            runtime_pipeline
                .get("service_driver")
                .and_then(|driver| driver.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("PLANNED")
        );
        assert_eq!(
            runtime_pipeline
                .get("health")
                .and_then(|health| health.get("status"))
                .and_then(serde_json::Value::as_str),
            Some("deferred")
        );
        if release.migrations.is_empty() {
            assert_eq!(
                runtime_pipeline
                    .get("migrations")
                    .and_then(|migrations| migrations.get("status"))
                    .and_then(serde_json::Value::as_str),
                Some("none")
            );
        } else {
            assert_eq!(
                runtime_pipeline
                    .get("migrations")
                    .and_then(|migrations| migrations.get("status"))
                    .and_then(serde_json::Value::as_str),
                Some("applied")
            );
        }
        assert_eq!(
            store
                .get_host_service(host_ip, service_name)
                .expect("get stack host service")
                .expect("stack host service")
                .version,
            release.version
        );
        assert!(
            store
                .get_endpoint(&endpoint)
                .expect("get stack endpoint")
                .is_some(),
            "{service_name} endpoint should exist after install"
        );
        assert_eq!(
            store.host_services().len(),
            index + 1,
            "stack installs should accumulate host service state"
        );
    }

    assert_eq!(store.services().len(), service_paths.len());
    assert_eq!(store.host_services().len(), service_paths.len());
    assert_eq!(store.endpoints().len(), service_paths.len());
    assert_eq!(store.service_releases().len(), service_paths.len());
    assert_eq!(store.service_routes().len(), total_routes);
    let api_surfaces = store.service_api_surfaces();
    assert_eq!(api_surfaces.len(), total_api_surfaces);
    for (service_name, api_id) in [
        ("auth-service", "auth.permission.check"),
        ("gateway", "gateway.health"),
        ("gateway", "gateway.routes.reload"),
        ("judge-api", "judge.queue.status"),
        ("problem-service", "problem.problem.read"),
    ] {
        assert!(
            api_surfaces
                .iter()
                .any(|api| api.service_name == service_name && api.api_id == api_id),
            "{service_name} release.install should register API surface {api_id}"
        );
    }
    assert!(
        store.service_frontend_entries().len() >= total_frontend,
        "frontend registry should contain at least every enabled frontend entry"
    );
    for service_name in [
        "auth-service",
        "gateway",
        "judge-api",
        "problem-service",
        "user-service",
    ] {
        assert!(
            store
                .service_frontend_entries()
                .iter()
                .any(|frontend| frontend.service_name == service_name),
            "{service_name} release.install should register frontend entry"
        );
    }
    assert_eq!(store.service_migration_records().len(), total_migrations);
    assert!(
        store
            .service_migration_records()
            .iter()
            .all(|record| record.status == "applied")
    );
    assert_eq!(store.service_permission_records().len(), total_permissions);
    assert!(
        store
            .service_permission_records()
            .iter()
            .any(|permission| permission.service_name == "auth-service"
                && permission.permission_key == "auth.admin")
    );
    assert!(
        store
            .service_permission_records()
            .iter()
            .any(|permission| permission.service_name == "judge-api"
                && permission.permission_key == "judge.submit")
    );
    assert!(
        store
            .service_permission_records()
            .iter()
            .any(|permission| permission.service_name == "problem-service"
                && permission.permission_key == "problem.view")
    );
    assert_eq!(store.service_redis_resources().len(), total_redis);
    assert!(
        store
            .service_redis_resources()
            .iter()
            .any(|redis| redis.service_name == "judge-worker" && redis.kind == "consumer-group")
    );
    assert_eq!(store.service_storage_resources().len(), total_storage);
    assert!(
        store
            .service_storage_resources()
            .iter()
            .any(|storage| storage.service_name == "storage-service"
                && storage.bucket == "submissions")
    );
    assert!(
        store.service_routes().iter().any(|route| {
            route.target_service_name == "judge-api"
                && route.target_type == "endpoint-group"
                && route
                    .target_selector
                    .get("group")
                    .and_then(serde_json::Value::as_str)
                    == Some("judge-api[*]")
        }),
        "gateway route registry should retain judge-api endpoint-group target"
    );
    assert_eq!(
        auth_calls.lock().expect("auth calls").len(),
        service_paths.len()
    );
    assert_eq!(
        auth_calls
            .lock()
            .expect("auth calls")
            .iter()
            .map(|request| request.permissions.len())
            .sum::<usize>(),
        total_permissions
    );
    assert_eq!(
        redis_calls.lock().expect("redis calls").len(),
        releases
            .iter()
            .filter(|(_, release)| !release.redis.is_empty())
            .count()
    );
    assert_eq!(
        redis_calls
            .lock()
            .expect("redis calls")
            .iter()
            .map(|request| request.resources.len())
            .sum::<usize>(),
        total_redis
    );
    assert_eq!(
        storage_calls.lock().expect("storage calls").len(),
        releases
            .iter()
            .filter(|(_, release)| !release.storage.is_empty())
            .count()
    );
    assert_eq!(
        storage_calls
            .lock()
            .expect("storage calls")
            .iter()
            .map(|request| request.resources.len())
            .sum::<usize>(),
        total_storage
    );
    assert_eq!(
        migration_calls.lock().expect("migration calls").len(),
        releases
            .iter()
            .filter(|(_, release)| !release.migrations.is_empty())
            .count()
    );
    assert_eq!(
        migration_calls
            .lock()
            .expect("migration calls")
            .iter()
            .map(|request| request.migrations.len())
            .sum::<usize>(),
        total_migrations
    );
    assert_eq!(
        gateway_calls.lock().expect("gateway calls").len(),
        service_paths.len(),
        "gateway route publisher refreshes once per service install"
    );
    assert_eq!(
        gateway_calls
            .lock()
            .expect("gateway calls")
            .iter()
            .map(|request| request.routes.len())
            .sum::<usize>(),
        total_routes
    );
    assert_eq!(
        node_calls.lock().expect("node calls").len(),
        service_paths.len()
    );
    assert!(
        node_calls
            .lock()
            .expect("node calls")
            .iter()
            .all(|request| request.endpoint.endpoint.ends_with(&request.service.id))
    );
}

#[test]
fn release_install_service_driver_execution_is_explicit_and_fail_fast() {
    let _guard = DOCKER_BINARY_ENV_LOCK.lock().expect("docker env lock");
    let previous = std::env::var("OJOS_ORCHESTRATOR_DOCKER_BINARY").ok();
    unsafe {
        std::env::set_var(
            "OJOS_ORCHESTRATOR_DOCKER_BINARY",
            "ojos-docker-compose-missing",
        );
    }

    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let mut release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    release.runtime = ReleaseRuntimeDecl {
        kind: "image".to_string(),
        image: String::new(),
        binary: String::new(),
        system_service: String::new(),
        command: String::new(),
        args: Vec::new(),
        working_dir: String::new(),
        env: BTreeMap::new(),
    };
    let request = ActionRequest::new(
        "op-release-install-driver-exec",
        "release.install",
        [
            ("service_id".to_string(), "gateway".to_string()),
            ("execute_service_driver".to_string(), "true".to_string()),
        ]
        .into_iter()
        .collect(),
    );
    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");

    OperationExecutor::new(&mut store)
        .with_service_driver_execution_enabled()
        .apply("op-release-install-driver-exec")
        .expect_err("missing docker binary should fail explicit release install execution");

    let failed = store
        .operation("op-release-install-driver-exec")
        .expect("stored operation");
    assert_eq!(failed.status, OperationStatus::Failed);
    assert!(
        failed
            .error_message
            .contains("fixed command failed to start"),
        "unexpected release.install driver error: {}",
        failed.error_message
    );
    assert!(
        store
            .operation_logs("op-release-install-driver-exec")
            .iter()
            .any(|record| record.step_id == "driver:release.install"
                && record
                    .data
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    == Some("release.install"))
    );

    unsafe {
        match previous {
            Some(value) => std::env::set_var("OJOS_ORCHESTRATOR_DOCKER_BINARY", value),
            None => std::env::remove_var("OJOS_ORCHESTRATOR_DOCKER_BINARY"),
        }
    }
}

#[test]
fn release_install_loads_local_release_package_when_loader_is_configured() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let operation = release_install_operation_with_release(
        "op-release-package-load",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");
    OperationExecutor::with_runtime_provisioners_and_release_loader(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        LocalReleasePackageLoader::new(&root),
    )
    .apply("op-release-package-load")
    .expect("apply release install");

    let logs = store.operation_logs("op-release-package-load");
    assert!(logs.iter().any(|log| {
        log.step_id == "release-package:gateway"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("loaded")
            && log
                .data
                .get("manifest_loaded")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:gateway"
            && log
                .data
                .get("release_package")
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str)
                == Some("loaded")
    }));
}

#[test]
fn release_install_dispatches_to_node_when_dispatcher_is_configured() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let operation = release_install_operation_with_release(
        "op-release-node-dispatch",
        &service,
        Some(&release),
        &[],
        "10.77.0.2",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = RecordingNodeServiceDispatcher {
        calls: calls.clone(),
        result: NodeServiceDispatchResult {
            status: "dispatched".to_string(),
            message: "recorded by test node dispatcher".to_string(),
            endpoint: "http://node-orchestrator.test".to_string(),
            accepted: true,
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_runtime_provisioners_release_loader_and_node_dispatcher(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        dispatcher,
    )
    .apply("op-release-node-dispatch")
    .expect("apply release install with node dispatch");

    let calls = calls.lock().expect("node dispatch calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].service.id, "gateway");
    assert_eq!(calls[0].host_service.host_ip, "10.77.0.2");
    assert_eq!(calls[0].endpoint.endpoint, "10.77.0.2:8080:gateway");
    assert_eq!(
        calls[0]
            .release
            .as_ref()
            .map(|manifest| manifest.service_name.as_str()),
        Some("gateway")
    );
    assert_eq!(
        calls[0]
            .rendered_config
            .get("external_steps")
            .and_then(|steps| steps.get("node_dispatch"))
            .and_then(serde_json::Value::as_str),
        Some("planned")
    );
    let host_service = store
        .get_host_service("10.77.0.2", "gateway")
        .expect("get host service")
        .expect("host service stored");
    assert_eq!(
        host_service
            .config
            .get("external_steps")
            .and_then(|steps| steps.get("node_dispatch"))
            .and_then(serde_json::Value::as_str),
        Some("dispatched")
    );
    assert_eq!(
        host_service
            .config
            .get("external_steps")
            .and_then(|steps| steps.get("node"))
            .and_then(|node| node.get("accepted"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    let logs = store.operation_logs("op-release-node-dispatch");
    assert!(logs.iter().any(|log| {
        log.step_id == "node-dispatch:gateway"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("dispatched")
            && log
                .data
                .get("accepted")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:gateway"
            && log
                .data
                .get("node_dispatch")
                .and_then(serde_json::Value::as_str)
                == Some("dispatched")
    }));
}

#[test]
fn release_install_fails_when_configured_node_dispatch_fails() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let operation = release_install_operation_with_release(
        "op-release-node-dispatch-fails",
        &service,
        Some(&release),
        &[],
        "10.77.0.3",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = RecordingNodeServiceDispatcher {
        calls: calls.clone(),
        result: NodeServiceDispatchResult {
            status: "FAILED".to_string(),
            message: "node service driver failed".to_string(),
            endpoint: "10.77.0.3:8080:gateway".to_string(),
            accepted: true,
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");

    let err = OperationExecutor::with_runtime_provisioners_release_loader_and_node_dispatcher(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        dispatcher,
    )
    .apply("op-release-node-dispatch-fails")
    .expect_err("node dispatch failure should fail release install");

    assert!(err.to_string().contains("node dispatch failed"));
    let calls = calls.lock().expect("node dispatch calls");
    assert_eq!(calls.len(), 1);
    let operation = store
        .get_operation("op-release-node-dispatch-fails")
        .expect("get operation")
        .expect("operation stored");
    assert_eq!(operation.status, OperationStatus::Failed);
    assert!(
        operation
            .error_message
            .contains("node dispatch failed: node service driver failed")
    );
    let logs = store.operation_logs("op-release-node-dispatch-fails");
    assert!(logs.iter().any(|log| {
        log.step_id == "node-dispatch:gateway"
            && log.level == "error"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("FAILED")
    }));
    assert!(logs.iter().any(|log| {
        log.step_id.is_empty()
            && log.level == "error"
            && log.message.contains("node dispatch failed")
    }));
}

#[test]
fn release_install_publishes_gateway_routes_when_publisher_is_configured() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let operation = release_install_operation_with_release(
        "op-release-gateway-publish",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let publisher = RecordingGatewayRoutePublisher {
        calls: calls.clone(),
        result: GatewayRoutePublishResult {
            status: "published".to_string(),
            message: "recorded by test gateway publisher".to_string(),
            endpoint: "http://gateway.test".to_string(),
            route_count: 0,
            reloaded: true,
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_runtime_provisioners_release_loader_gateway_publisher_and_node_dispatcher(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        publisher,
        DeferredNodeServiceDispatcher,
    )
    .apply("op-release-gateway-publish")
    .expect("apply release install with gateway route publish");

    let calls = calls.lock().expect("gateway publisher calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation_id, "op-release-gateway-publish");
    assert_eq!(calls[0].service_name, "gateway");
    assert_eq!(calls[0].routes.len(), release.routes.len());
    assert_eq!(calls[0].api_count, release.apis.len());
    assert!(calls[0].force_reload);
    assert!(
        calls[0]
            .routes
            .iter()
            .any(|route| route.target_service_name == "gateway"
                && route.target_type == "endpoint-group"
                && route
                    .target_selector
                    .get("group")
                    .and_then(serde_json::Value::as_str)
                    == Some("gateway[*]")),
        "gateway route publisher should receive release route records"
    );
    let logs = store.operation_logs("op-release-gateway-publish");
    assert!(logs.iter().any(|log| {
        log.step_id == "gateway_reload:gateway"
            && log
                .message
                .contains("[OK] gateway reload triggered by orchestrator")
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("published")
            && log
                .data
                .get("reloaded")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:gateway"
            && log
                .data
                .get("gateway_route_update")
                .and_then(serde_json::Value::as_str)
                == Some("published")
            && log
                .data
                .get("gateway_route_publish")
                .and_then(|value| value.get("reloaded"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));
}

#[test]
fn release_install_api_surface_only_release_triggers_gateway_reload() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/storage-service/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/storage-service/release.yaml"))
            .unwrap();
    assert!(
        release.routes.is_empty(),
        "storage release should be API-only for gateway reload"
    );
    assert!(
        !release.apis.is_empty(),
        "storage release should declare API surface"
    );

    let endpoint = "127.0.0.1:19280:storage-service";
    let operation = release_install_operation_with_release(
        "op-release-storage-api-reload",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        Some(endpoint),
        serde_json::json!({
            "external_service_running": true,
            "gateway_node_id": "child-node"
        }),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed storage release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let publisher = RecordingGatewayRoutePublisher {
        calls: calls.clone(),
        result: GatewayRoutePublishResult {
            status: "published".to_string(),
            message: "recorded API surface reload".to_string(),
            endpoint: "http://gateway.test".to_string(),
            route_count: 0,
            reloaded: true,
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store
        .upsert_node(NodeRecord {
            node_id: "root-node".to_string(),
            host_ip: "127.0.0.1".to_string(),
            parent_node_id: String::new(),
            role: "root".to_string(),
            labels: serde_json::json!({}),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("root node");
    store
        .upsert_node(NodeRecord {
            node_id: "child-node".to_string(),
            host_ip: "127.0.0.2".to_string(),
            parent_node_id: "root-node".to_string(),
            role: "node".to_string(),
            labels: serde_json::json!({}),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("child node");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_runtime_provisioners_release_loader_gateway_publisher_and_node_dispatcher(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        publisher,
        DeferredNodeServiceDispatcher,
    )
    .apply("op-release-storage-api-reload")
    .expect("apply API-only storage release install");

    let calls = calls.lock().expect("gateway publisher calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].service_name, "storage-service");
    assert!(calls[0].routes.is_empty());
    assert_eq!(calls[0].node_id, "child-node");
    assert_eq!(calls[0].api_count, release.apis.len());
    assert!(calls[0].force_reload);
    for api_id in [
        "storage.object.put",
        "storage.object.get",
        "storage.object.head",
    ] {
        assert!(
            calls[0]
                .effective_routes
                .iter()
                .any(|route| route.api_id == api_id
                    && route.provider_endpoint == endpoint
                    && route.provider_node_id == "root-node"
                    && route.node_id == "child-node"
                    && route.status == "running"),
            "gateway publisher should receive effective route {api_id}"
        );
    }
    drop(calls);

    let routes = store
        .effective_api_routes("child-node")
        .expect("child effective API routes");
    for api_id in [
        "storage.object.put",
        "storage.object.get",
        "storage.object.head",
    ] {
        assert!(
            routes.iter().any(|route| {
                route.api_id == api_id
                    && route.provider_endpoint == endpoint
                    && route.provider_node_id == "root-node"
                    && route.status == "running"
            }),
            "release.install should produce running effective API route {api_id}"
        );
    }

    let logs = store.operation_logs("op-release-storage-api-reload");
    assert!(logs.iter().any(|log| {
        log.step_id == "gateway_reload:storage-service"
            && log
                .message
                .contains("[OK] gateway reload triggered by orchestrator")
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("published")
            && log
                .data
                .get("api_count")
                .and_then(serde_json::Value::as_u64)
                == Some(release.apis.len() as u64)
            && log
                .data
                .get("force_reload")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:storage-service"
            && log
                .data
                .get("gateway_route_update")
                .and_then(serde_json::Value::as_str)
                == Some("published")
            && log
                .data
                .get("host_service")
                .and_then(serde_json::Value::as_str)
                == Some("created")
    }));
}

#[test]
fn release_install_fails_when_gateway_reload_fails() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/storage-service/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/storage-service/release.yaml"))
            .unwrap();
    let operation = release_install_operation_with_release(
        "op-release-storage-gateway-reload-fails",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        Some("127.0.0.1:19280:storage-service"),
        serde_json::json!({
            "external_service_running": true,
            "gateway_node_id": "child-node"
        }),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed storage release install");

    let mut store = MemoryOrchestratorStore::new();
    store
        .upsert_node(NodeRecord {
            node_id: "root-node".to_string(),
            host_ip: "127.0.0.1".to_string(),
            parent_node_id: String::new(),
            role: "root".to_string(),
            labels: serde_json::json!({}),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("root node");
    store
        .upsert_node(NodeRecord {
            node_id: "child-node".to_string(),
            host_ip: "127.0.0.2".to_string(),
            parent_node_id: "root-node".to_string(),
            role: "node".to_string(),
            labels: serde_json::json!({}),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("child node");
    store.put_operation(operation).expect("put operation");

    let err = OperationExecutor::with_runtime_provisioners_release_loader_gateway_publisher_and_node_dispatcher(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        DeferredReleasePackageLoader,
        FailingGatewayRoutePublisher {
            message: "gateway route publish failed: http 503".to_string(),
        },
        DeferredNodeServiceDispatcher,
    )
    .apply("op-release-storage-gateway-reload-fails")
    .expect_err("gateway reload failure must fail release.install");

    assert!(err.to_string().contains("gateway route publish failed"));
    let operation = store
        .operation("op-release-storage-gateway-reload-fails")
        .expect("failed operation");
    assert_eq!(operation.status, OperationStatus::Failed);
    assert!(
        operation
            .error_message
            .contains("gateway route publish failed")
    );
    let logs = store.operation_logs("op-release-storage-gateway-reload-fails");
    assert!(logs.iter().any(|log| {
        log.step_id.is_empty()
            && log.level == "error"
            && log.message.contains("gateway route publish failed")
    }));
}

#[test]
fn release_install_fails_when_loaded_release_package_differs_from_operation_manifest() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let mut release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    release.source.url = "services/auth-service".to_string();
    let operation = release_install_operation_with_release(
        "op-release-package-mismatch",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");
    let err = OperationExecutor::with_runtime_provisioners_and_release_loader(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        LocalReleasePackageLoader::new(&root),
    )
    .apply("op-release-package-mismatch")
    .expect_err("mismatched loaded release package should fail install");
    assert!(
        err.to_string().contains("does not match requested")
            || err
                .to_string()
                .contains("differs from operation release_manifest")
    );
    let failed = store
        .operation("op-release-package-mismatch")
        .expect("stored operation");
    assert_eq!(failed.status, OperationStatus::Failed);
}

#[test]
fn local_release_package_loader_fetches_http_release_yaml() {
    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let release_yaml =
        fs::read_to_string(root.join("services/gateway/release.yaml")).expect("read release yaml");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind release package listener");
    let url = format!(
        "http://{}/release.yaml",
        listener.local_addr().expect("release package addr")
    );
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept release package request");
        let mut buffer = [0_u8; 1024];
        let _ = stream
            .read(&mut buffer)
            .expect("read release package request");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/yaml\r\nContent-Length: {}\r\n\r\n{}",
            release_yaml.len(),
            release_yaml
        );
        stream
            .write_all(response.as_bytes())
            .expect("write release package response");
    });

    let result = LocalReleasePackageLoader::new(&root)
        .load_release_package(&ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: url.clone(),
            expected_checksum: None,
            expected_manifest: Some(release),
        })
        .expect("http release package load");
    handle.join().expect("release package listener thread");
    assert_eq!(result.status, "loaded");
    assert_eq!(result.source_url, url);
    assert!(result.manifest_loaded);
    assert!(result.checksum.starts_with("sha256:"));
}

#[test]
fn local_release_package_loader_fetches_http_zip_release_package() {
    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let release_yaml =
        fs::read_to_string(root.join("services/gateway/release.yaml")).expect("read release yaml");
    let package = zip_release_package(&[("gateway-release/release.yaml", release_yaml.as_str())]);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind release package listener");
    let url = format!(
        "http://{}/gateway-release.zip",
        listener.local_addr().expect("release package addr")
    );
    let handle = thread::spawn({
        let package = package.clone();
        move || serve_bytes_once(listener, "application/zip", &package)
    });

    let result = LocalReleasePackageLoader::new(&root)
        .load_release_package(&ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: url.clone(),
            expected_checksum: None,
            expected_manifest: Some(release),
        })
        .expect("http zip release package load");
    handle.join().expect("release package listener thread");
    assert_eq!(result.status, "loaded");
    assert_eq!(result.source_url, url);
    assert!(result.manifest_loaded);
    assert!(result.checksum.starts_with("sha256:"));
}

#[test]
fn local_release_package_loader_fetches_local_zip_release_package() {
    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let release_yaml =
        fs::read_to_string(root.join("services/gateway/release.yaml")).expect("read release yaml");
    let package = zip_release_package(&[("gateway-release/release.yaml", release_yaml.as_str())]);
    let dir = tempdir().expect("release package tempdir");
    fs::write(dir.path().join("gateway-release.zip"), &package).expect("write release package");

    let result = LocalReleasePackageLoader::new(dir.path())
        .load_release_package(&ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: "gateway-release.zip".to_string(),
            expected_checksum: None,
            expected_manifest: Some(release),
        })
        .expect("local zip release package load");
    assert_eq!(result.status, "loaded");
    assert_eq!(result.source_url, "gateway-release.zip");
    assert!(result.manifest_loaded);
    assert_eq!(result.checksum, format!("sha256:{}", sha256_hex(&package)));
}

#[test]
fn local_release_package_loader_rejects_archive_without_release_yaml() {
    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let package = zip_release_package(&[("gateway-release/README.md", "missing manifest")]);
    let dir = tempdir().expect("release package tempdir");
    fs::write(dir.path().join("missing-release.zip"), &package).expect("write release package");

    let err = LocalReleasePackageLoader::new(dir.path())
        .load_release_package(&ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: "missing-release.zip".to_string(),
            expected_checksum: None,
            expected_manifest: Some(release),
        })
        .expect_err("archive without release.yaml should fail");
    assert!(err.to_string().contains("does not contain release.yaml"));
}

#[test]
fn release_install_fails_on_release_package_checksum_mismatch() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let mut release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let release_yaml =
        fs::read_to_string(root.join("services/gateway/release.yaml")).expect("read release yaml");
    let package = zip_release_package(&[("gateway-release/release.yaml", release_yaml.as_str())]);
    let dir = tempdir().expect("release package tempdir");
    fs::write(dir.path().join("gateway-release.zip"), &package).expect("write release package");
    release.source.url = "gateway-release.zip".to_string();
    release.source.checksum = String::new();
    let operation = release_install_operation_with_release(
        "op-release-package-checksum-mismatch",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");
    let mut operation = store
        .operation("op-release-package-checksum-mismatch")
        .expect("stored operation")
        .clone();
    operation.request["release_checksum"] = serde_json::json!(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    );
    store
        .put_operation(operation)
        .expect("put patched operation");
    let err = OperationExecutor::with_runtime_provisioners_and_release_loader(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
        LocalReleasePackageLoader::new(dir.path()),
    )
    .apply("op-release-package-checksum-mismatch")
    .expect_err("checksum mismatch should fail install");
    assert!(
        err.to_string()
            .contains("release package checksum mismatch")
    );
    let failed = store
        .operation("op-release-package-checksum-mismatch")
        .expect("stored operation");
    assert_eq!(failed.status, OperationStatus::Failed);
}

#[test]
fn local_release_package_loader_rejects_zip_path_traversal() {
    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let package = zip_release_package(&[("../release.yaml", "service_name: gateway\n")]);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind release package listener");
    let url = format!(
        "http://{}/unsafe-release.zip",
        listener.local_addr().expect("release package addr")
    );
    let handle = thread::spawn({
        let package = package.clone();
        move || serve_bytes_once(listener, "application/zip", &package)
    });

    let err = LocalReleasePackageLoader::new(&root)
        .load_release_package(&ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: url,
            expected_checksum: None,
            expected_manifest: Some(release),
        })
        .expect_err("unsafe zip release package should fail load");
    handle.join().expect("release package listener thread");
    assert!(err.to_string().contains("escapes") || err.to_string().contains("traversal"));
}

#[test]
fn local_release_package_loader_fails_on_http_error() {
    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind release package listener");
    let url = format!(
        "http://{}/missing.yaml",
        listener.local_addr().expect("release package addr")
    );
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept release package request");
        let mut buffer = [0_u8; 1024];
        let _ = stream
            .read(&mut buffer)
            .expect("read release package request");
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            .expect("write release package response");
    });

    let err = LocalReleasePackageLoader::new(&root)
        .load_release_package(&ReleasePackageLoadRequest {
            service_name: release.service_name.clone(),
            version: release.version.clone(),
            source_url: url,
            expected_checksum: None,
            expected_manifest: Some(release),
        })
        .expect_err("http release package error should fail load");
    handle.join().expect("release package listener thread");
    assert!(err.to_string().contains("http 404"));
}

fn serve_bytes_once(listener: TcpListener, content_type: &str, body: &[u8]) {
    let (mut stream, _) = listener.accept().expect("accept release package request");
    let mut buffer = [0_u8; 1024];
    let _ = stream
        .read(&mut buffer)
        .expect("read release package request");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
        content_type,
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write release package response headers");
    stream
        .write_all(body)
        .expect("write release package response body");
}

fn zip_release_package(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = std::io::Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default();
    for (path, content) in entries {
        archive
            .start_file(path, options)
            .expect("start release package zip entry");
        archive
            .write_all(content.as_bytes())
            .expect("write release package zip entry");
    }
    archive
        .finish()
        .expect("finish release package zip")
        .into_inner()
}

#[test]
fn release_install_registers_permissions_with_auth_registrar() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let request = ActionRequest::new(
        "op-release-auth-registration",
        "release.install",
        [("service_id".to_string(), "gateway".to_string())]
            .into_iter()
            .collect(),
    );
    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release-aware operation");
    let confirmed = confirm_operation(&operation).expect("confirm release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let registrar = RecordingAuthPermissionRegistrar {
        calls: Arc::clone(&calls),
    };
    let mut store = MemoryOrchestratorStore::new();
    seed_storage_identity_api_surfaces(&mut store);
    store.put_operation(confirmed).expect("put operation");

    OperationExecutor::with_endpoint_probe_and_auth_registrar(
        &mut store,
        StaticEndpointProbe,
        registrar,
    )
    .apply("op-release-auth-registration")
    .expect("apply release install");

    let calls = calls.lock().expect("auth permission calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].service_name, "gateway");
    assert_eq!(calls[0].permissions, release.permissions);
    drop(calls);

    let logs = store.operation_logs("op-release-auth-registration");
    assert!(logs.iter().any(|log| {
        log.step_id == "auth-permissions:gateway"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("registered")
            && log
                .data
                .get("registered")
                .and_then(serde_json::Value::as_u64)
                == Some(release.permissions.len() as u64)
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:gateway"
            && log
                .data
                .get("permission_registration")
                .and_then(serde_json::Value::as_str)
                == Some("registered")
            && log
                .data
                .get("auth_permission_registration")
                .and_then(|value| value.get("endpoint"))
                .and_then(serde_json::Value::as_str)
                == Some("http://auth-service.test")
    }));
}

#[test]
fn release_install_registers_service_identity_grants_with_auth_registrar() {
    let mut service = valid_service();
    service.id = "judge-worker".to_string();
    service.name = "Judge Worker".to_string();
    service.kind = "backend-worker".to_string();
    service.permissions = vec!["judge.worker".to_string()];
    let mut release = valid_release_for_service(&service);
    release.routes = Vec::new();
    release.required_apis = vec![
        "storage.object.get".to_string(),
        "storage.object.put".to_string(),
    ];
    release.service_identity = ReleaseServiceIdentityDecl {
        service_name: "judge-worker".to_string(),
        allowed_apis: release.required_apis.clone(),
    };

    let operation = release_install_operation_with_release(
        "op-release-service-identity-registration",
        &service,
        Some(&release),
        &[],
        "127.0.0.2",
        Some("127.0.0.2:9101:judge-worker"),
        serde_json::json!({"external_service_running": true}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let registrar = RecordingAuthPermissionRegistrar {
        calls: Arc::clone(&calls),
    };
    let mut store = MemoryOrchestratorStore::new();
    store
        .upsert_service_api_surface(ServiceApiSurface {
            service_name: "storage-service".to_string(),
            version: "0.1.0".to_string(),
            api_id: "storage.object.get".to_string(),
            protocol: "http".to_string(),
            port_name: "http".to_string(),
            path_prefix: "/api/storage/objects".to_string(),
            methods: vec!["GET".to_string()],
            visibility: "descendants".to_string(),
            auth_mode: "service".to_string(),
            permission: "storage.object.read".to_string(),
            stability: "stable".to_string(),
            api_version: "v1".to_string(),
            rate_limit: String::new(),
            timeout: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put get surface");
    store
        .upsert_service_api_surface(ServiceApiSurface {
            service_name: "storage-service".to_string(),
            version: "0.1.0".to_string(),
            api_id: "storage.object.put".to_string(),
            protocol: "http".to_string(),
            port_name: "http".to_string(),
            path_prefix: "/api/storage/objects".to_string(),
            methods: vec!["PUT".to_string()],
            visibility: "descendants".to_string(),
            auth_mode: "service".to_string(),
            permission: "storage.object.write".to_string(),
            stability: "stable".to_string(),
            api_version: "v1".to_string(),
            rate_limit: String::new(),
            timeout: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put put surface");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_endpoint_probe_and_auth_registrar(
        &mut store,
        StaticEndpointProbe,
        registrar,
    )
    .apply("op-release-service-identity-registration")
    .expect("apply release install");

    let calls = calls.lock().expect("auth permission calls");
    assert_eq!(calls.len(), 1);
    let identity = calls[0]
        .service_identity
        .as_ref()
        .expect("service identity registration");
    assert_eq!(identity.service_name, "judge-worker");
    assert_eq!(identity.allowed_apis, release.required_apis);
    assert!(identity.grants.contains(&AuthServiceIdentityGrant {
        api_id: "storage.object.get".to_string(),
        permission: "storage.object.read".to_string(),
    }));
    assert!(identity.grants.contains(&AuthServiceIdentityGrant {
        api_id: "storage.object.put".to_string(),
        permission: "storage.object.write".to_string(),
    }));
}

#[test]
fn http_auth_permission_registrar_posts_release_permissions_to_auth_service() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local auth test listener");
    let endpoint = format!("http://{}", listener.local_addr().expect("auth test addr"));
    let captured = Arc::new(Mutex::new(String::new()));
    let captured_for_thread = Arc::clone(&captured);
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept auth request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("auth request read timeout");
        let mut body = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let bytes = stream.read(&mut buffer).expect("read auth request");
            if bytes == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..bytes]);
            if http_request_body_is_complete(&body) {
                break;
            }
        }
        *captured_for_thread.lock().expect("captured auth request") =
            String::from_utf8(body).expect("auth request utf8");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 25\r\nContent-Type: application/json\r\n\r\n{\"code\":0,\"msg\":\"ok\"}\n",
            )
            .expect("write auth response");
    });

    let registrar = HttpAuthPermissionRegistrar::new(endpoint, "secret-admin-token")
        .with_timeout(Duration::from_secs(2));
    let request = AuthPermissionRegistration {
        service_name: "judge-api".to_string(),
        permissions: vec!["judge.submit".to_string(), "judge.read".to_string()],
        service_identity: Some(AuthServiceIdentityRegistration {
            service_name: "judge-api".to_string(),
            allowed_apis: vec!["storage.object.get".to_string()],
            grants: vec![AuthServiceIdentityGrant {
                api_id: "storage.object.get".to_string(),
                permission: "storage.object.read".to_string(),
            }],
        }),
    };
    let result = registrar
        .register_permissions(&request)
        .expect("auth permission registration");
    handle.join().expect("auth listener thread");

    assert_eq!(result.status, "registered");
    assert_eq!(result.registered, 2);
    let request_text = captured.lock().expect("captured auth request");
    assert!(request_text.starts_with("POST /auth/admin/services/judge-api/permissions "));
    assert!(
        request_text
            .to_ascii_lowercase()
            .contains("authorization: bearer secret-admin-token")
    );
    assert!(request_text.contains("\"code\":\"judge.submit\""));
    assert!(request_text.contains("\"code\":\"judge.read\""));
    assert!(request_text.contains("\"default_role_bindings\":[]"));
    assert!(request_text.contains("\"service_identity\""));
    assert!(request_text.contains("\"service_name\":\"judge-api\""));
    assert!(request_text.contains("\"api_id\":\"storage.object.get\""));
    assert!(request_text.contains("\"permission\":\"storage.object.read\""));
    assert!(
        !request_text.contains("system.admin"),
        "orchestrator must not pre-seed unrelated permissions"
    );
}

#[test]
fn release_install_executes_migrations_with_runner() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: "len:18".to_string(),
        destructive: false,
    }];
    let operation = release_install_operation_with_release(
        "op-release-migration-runner",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .expect("release install operation");
    let confirmed = confirm_operation(&operation).expect("confirm release install");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runner = RecordingMigrationRunner {
        calls: Arc::clone(&calls),
        result: MigrationExecutionResult {
            status: "applied".to_string(),
            message: "recorded by test migration runner".to_string(),
            runner: "recording".to_string(),
            dry_run: false,
            executed: vec![MigrationExecutionRecord {
                migration_version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: "len:18".to_string(),
                status: "applied".to_string(),
                applied_at: "applied".to_string(),
                message: "applied by test".to_string(),
            }],
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(confirmed).expect("put operation");

    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
    )
    .apply("op-release-migration-runner")
    .expect("apply release install");

    let calls = calls.lock().expect("migration runner calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].service_name, "demo-api");
    assert_eq!(calls[0].migrations[0].version, "0001");
    drop(calls);

    let records = store.service_migration_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "applied");
    assert_eq!(records[0].applied_at, "applied");
    let logs = store.operation_logs("op-release-migration-runner");
    assert!(logs.iter().any(|log| {
        log.step_id == "migrations:demo-api"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("applied")
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:demo-api"
            && log
                .data
                .get("migrations")
                .and_then(|value| value.get("status"))
                .and_then(serde_json::Value::as_str)
                == Some("applied")
    }));
}

#[test]
fn release_install_marks_failed_migration_and_fails_operation() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: String::new(),
        destructive: false,
    }];
    let operation = release_install_operation_with_release(
        "op-release-migration-failure",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runner = FailingMigrationRunner {
        calls: Arc::clone(&calls),
        message: "synthetic migration failure".to_string(),
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
    )
    .apply("op-release-migration-failure")
    .expect_err("migration failure should fail release install");

    assert_eq!(calls.lock().expect("migration calls").len(), 1);
    let failed = store
        .get_operation("op-release-migration-failure")
        .expect("get operation")
        .expect("operation");
    assert_eq!(failed.status, OperationStatus::Failed);
    assert!(failed.error_message.contains("synthetic migration failure"));
    let records = store.service_migration_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "failed");
    let logs = store.operation_logs("op-release-migration-failure");
    assert!(logs.iter().any(|log| {
        log.step_id == "migrations:demo-api"
            && log.level == "error"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("failed")
    }));
}

#[test]
fn failed_release_install_rollback_removes_partial_pipeline_state() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.frontend = ReleaseFrontendDecl {
        enabled: true,
        route_prefix: "/demo-ui".to_string(),
        remote_entry: "/assets/demo-api/remoteEntry.js".to_string(),
        menu_items: vec!["demo".to_string()],
    };
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: "sha256:demo".to_string(),
        destructive: false,
    }];
    release.redis = vec![ReleaseRedisDecl {
        name: "judge-tasks".to_string(),
        kind: "stream".to_string(),
        usage: "demo judge task stream".to_string(),
    }];
    release.storage = vec![ReleaseStorageDecl {
        object_type: "submission-source".to_string(),
        bucket: "submissions".to_string(),
        path_prefix: "/demo".to_string(),
    }];
    let operation = release_install_operation_with_release(
        "op-release-install-partial-rollback",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");
    let runner = RecordingMigrationRunner {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: MigrationExecutionResult {
            status: "applied".to_string(),
            message: "recorded by test migration runner".to_string(),
            runner: "recording".to_string(),
            dry_run: false,
            executed: vec![MigrationExecutionRecord {
                migration_version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: "sha256:demo".to_string(),
                status: "applied".to_string(),
                applied_at: "applied".to_string(),
                message: "applied by test".to_string(),
            }],
        },
    };
    let dispatcher = RecordingNodeServiceDispatcher {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: NodeServiceDispatchResult {
            status: "FAILED".to_string(),
            message: "node runtime rejected install".to_string(),
            endpoint: "127.0.0.1:18080:demo-api".to_string(),
            accepted: true,
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_runtime_provisioners_release_loader_and_node_dispatcher(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
        DeferredReleasePackageLoader,
        dispatcher,
    )
    .apply("op-release-install-partial-rollback")
    .expect_err("node dispatch failure should fail release install");

    assert!(store.get_service("demo-api").unwrap().is_some());
    assert!(
        store
            .get_host_service("127.0.0.1", "demo-api")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_endpoint("127.0.0.1:18080:demo-api")
            .unwrap()
            .is_some()
    );
    assert!(!store.service_routes().is_empty());
    assert!(!store.service_migration_records().is_empty());
    assert!(!store.service_permission_records().is_empty());

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-release-install-partial-rollback")
        .expect("rollback failed release install");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    assert!(store.get_service("demo-api").unwrap().is_none());
    assert!(
        store
            .get_host_service("127.0.0.1", "demo-api")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_endpoint("127.0.0.1:18080:demo-api")
            .unwrap()
            .is_none()
    );
    assert!(store.service_routes().is_empty());
    assert!(store.service_migration_records().is_empty());
    assert!(store.service_permission_records().is_empty());
    assert!(store.service_frontend_entries().is_empty());
    assert!(store.service_redis_resources().is_empty());
    assert!(store.service_storage_resources().is_empty());
    assert!(store.rendered_service_configs().is_empty());
    assert!(store.service_releases().is_empty());
}

#[test]
fn destructive_release_migration_requires_confirmation() {
    let dir = tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join("migrations")).expect("migration dir");
    fs::create_dir_all(dir.path().join("services/demo-api/migrations")).expect("migration dir");
    fs::write(
        dir.path().join("services/demo-api/migrations/0001.sql"),
        "DROP TABLE demo;\n",
    )
    .expect("migration sql");
    let runner = LocalSqlMigrationRunner::new(dir.path()).with_dry_run(true);
    let err = runner
        .execute_migrations(&MigrationExecutionRequest {
            service_name: "demo-api".to_string(),
            migrations: vec![ReleaseMigrationDecl {
                version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: "len:17".to_string(),
                destructive: true,
            }],
            release_source_url: "local://services/demo-api".to_string(),
            dry_run: false,
            allow_destructive: false,
        })
        .expect_err("destructive migration requires explicit allowance");
    assert!(
        err.to_string()
            .contains("destructive migration 0001 requires")
    );
}

#[test]
fn local_sql_migration_runner_dry_run_validates_checksum() {
    let dir = tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join("services/demo-api/migrations")).expect("migration dir");
    fs::write(
        dir.path().join("services/demo-api/migrations/0001.sql"),
        "SELECT 1;\n",
    )
    .expect("migration sql");
    let runner = LocalSqlMigrationRunner::new(dir.path()).with_dry_run(true);

    let result = runner
        .execute_migrations(&MigrationExecutionRequest {
            service_name: "demo-api".to_string(),
            migrations: vec![ReleaseMigrationDecl {
                version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: "sha256:b4e0497804e46e0a0b0b8c31975b062152d551bac49c3c2e80932567b4085dcd"
                    .to_string(),
                destructive: false,
            }],
            release_source_url: "local://services/demo-api".to_string(),
            dry_run: false,
            allow_destructive: false,
        })
        .expect("dry-run migration validates");

    assert_eq!(result.status, "dry-run");
    assert_eq!(result.executed[0].status, "dry-run");
    let err = runner
        .execute_migrations(&MigrationExecutionRequest {
            service_name: "demo-api".to_string(),
            migrations: vec![ReleaseMigrationDecl {
                version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
                destructive: false,
            }],
            release_source_url: "local://services/demo-api".to_string(),
            dry_run: false,
            allow_destructive: false,
        })
        .expect_err("checksum mismatch should fail");
    assert!(err.to_string().contains("checksum mismatch"));
}

#[test]
fn local_sql_migration_runner_prefers_service_owned_database_url() {
    let runner = LocalSqlMigrationRunner::new(repo_root())
        .with_service_database_url(
            "auth-service",
            "postgres://postgres:auth@auth-db:5432/ojos_auth",
        )
        .with_service_database_url(
            "judge-api",
            "postgres://postgres:judge@judge-db:5432/ojos_judge",
        );

    assert_eq!(
        runner.database_url_for_service("auth-service").as_deref(),
        Some("postgres://postgres:auth@auth-db:5432/ojos_auth")
    );
    assert_eq!(
        runner.database_url_for_service("judge-api").as_deref(),
        Some("postgres://postgres:judge@judge-db:5432/ojos_judge")
    );
    assert_eq!(
        runner
            .database_url_for_service("problem-service")
            .as_deref(),
        None
    );
}

#[test]
fn release_install_skips_already_applied_migration_with_same_checksum() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: "len:18".to_string(),
        destructive: false,
    }];
    let operation = release_install_operation_with_release(
        "op-release-migration-idempotent",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runner = RecordingMigrationRunner {
        calls: Arc::clone(&calls),
        result: MigrationExecutionResult {
            status: "applied".to_string(),
            message: "should not be called".to_string(),
            runner: "recording".to_string(),
            dry_run: false,
            executed: Vec::new(),
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store
        .upsert_service_migration_record(ServiceMigrationRecord {
            service_name: "demo-api".to_string(),
            migration_version: "0001".to_string(),
            checksum: "len:18".to_string(),
            status: "applied".to_string(),
            applied_at: "2026-07-02T00:00:00Z".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("seed applied migration");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
    )
    .apply("op-release-migration-idempotent")
    .expect("apply release install");

    assert!(calls.lock().expect("migration calls").is_empty());
    let records = store.service_migration_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "applied");
    assert_eq!(records[0].applied_at, "2026-07-02T00:00:00Z");
    let logs = store.operation_logs("op-release-migration-idempotent");
    assert!(logs.iter().any(|log| {
        log.step_id == "migrations:demo-api"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("skipped")
            && log.data.get("runner").and_then(serde_json::Value::as_str) == Some("already-applied")
    }));
}

#[test]
fn repeated_release_install_runs_same_service_migration_once() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: "len:18".to_string(),
        destructive: false,
    }];
    let first_operation = release_install_operation_with_release(
        "op-release-migration-first-install",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        Some("127.0.0.1:18080:demo-api"),
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed first release install");
    let second_operation = release_install_operation_with_release(
        "op-release-migration-second-install",
        &service,
        Some(&release),
        &["demo-api".to_string()],
        "127.0.0.1",
        Some("127.0.0.1:18081:demo-api"),
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed second release install");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runner = RecordingMigrationRunner {
        calls: Arc::clone(&calls),
        result: MigrationExecutionResult {
            status: "applied".to_string(),
            message: "recorded by test migration runner".to_string(),
            runner: "recording".to_string(),
            dry_run: false,
            executed: vec![MigrationExecutionRecord {
                migration_version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: "len:18".to_string(),
                status: "applied".to_string(),
                applied_at: "applied".to_string(),
                message: "applied by test".to_string(),
            }],
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store
        .put_operation(first_operation)
        .expect("put first operation");
    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner.clone(),
    )
    .apply("op-release-migration-first-install")
    .expect("apply first release install");
    store
        .put_operation(second_operation)
        .expect("put second operation");
    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
    )
    .apply("op-release-migration-second-install")
    .expect("apply second release install");

    assert_eq!(calls.lock().expect("migration calls").len(), 1);
    let records = store.service_migration_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, "applied");
    let logs = store.operation_logs("op-release-migration-second-install");
    assert!(logs.iter().any(|log| {
        log.step_id == "migrations:demo-api"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("skipped")
            && log.data.get("runner").and_then(serde_json::Value::as_str) == Some("already-applied")
    }));
}

#[test]
fn release_install_fails_when_applied_migration_checksum_changes() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: "len:18".to_string(),
        destructive: false,
    }];
    let operation = release_install_operation_with_release(
        "op-release-migration-checksum-changed",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let runner = RecordingMigrationRunner {
        calls: Arc::clone(&calls),
        result: MigrationExecutionResult {
            status: "applied".to_string(),
            message: "should not be called".to_string(),
            runner: "recording".to_string(),
            dry_run: false,
            executed: Vec::new(),
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store
        .upsert_service_migration_record(ServiceMigrationRecord {
            service_name: "demo-api".to_string(),
            migration_version: "0001".to_string(),
            checksum: "len:17".to_string(),
            status: "applied".to_string(),
            applied_at: "2026-07-02T00:00:00Z".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("seed applied migration");
    store.put_operation(operation).expect("put operation");

    let err = OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
    )
    .apply("op-release-migration-checksum-changed")
    .expect_err("changed checksum should fail release install");

    assert!(calls.lock().expect("migration calls").is_empty());
    assert!(err.to_string().contains("already applied with checksum"));
}

#[test]
fn release_install_rollback_records_migration_rollback_unsupported() {
    let service = valid_service();
    let mut release = valid_release_for_service(&service);
    release.migrations = vec![ReleaseMigrationDecl {
        version: "0001".to_string(),
        path: "services/demo-api/migrations/0001.sql".to_string(),
        checksum: String::new(),
        destructive: false,
    }];
    let operation = release_install_operation_with_release(
        "op-release-migration-rollback",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        None,
        serde_json::json!({}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed release install");
    let runner = RecordingMigrationRunner {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: MigrationExecutionResult {
            status: "applied".to_string(),
            message: "recorded by test migration runner".to_string(),
            runner: "recording".to_string(),
            dry_run: false,
            executed: vec![MigrationExecutionRecord {
                migration_version: "0001".to_string(),
                path: "services/demo-api/migrations/0001.sql".to_string(),
                checksum: String::new(),
                status: "applied".to_string(),
                applied_at: "applied".to_string(),
                message: "applied by test".to_string(),
            }],
        },
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");
    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        DeferredStorageResourceProvisioner,
        runner,
    )
    .apply("op-release-migration-rollback")
    .expect("apply release install");

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-release-migration-rollback")
        .expect("rollback release install");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    let logs = store.operation_logs("op-release-migration-rollback");
    assert!(logs.iter().any(|log| {
        log.step_id == "migration-rollback:unsupported"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("unsupported")
    }));
}

#[test]
fn release_install_provisions_redis_resources_with_runtime_provisioner() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/judge-worker/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/judge-worker/release.yaml"))
            .unwrap();
    let request = ActionRequest::new(
        "op-release-redis-provision",
        "release.install",
        [("service_id".to_string(), "judge-worker".to_string())]
            .into_iter()
            .collect(),
    );
    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release-aware operation");
    let confirmed = confirm_operation(&operation).expect("confirm release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let redis = RecordingRedisResourceProvisioner {
        calls: Arc::clone(&calls),
    };
    let mut store = MemoryOrchestratorStore::new();
    seed_storage_identity_api_surfaces(&mut store);
    store.put_operation(confirmed).expect("put operation");

    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        redis,
        DeferredStorageResourceProvisioner,
        DeferredMigrationRunner,
    )
    .apply("op-release-redis-provision")
    .expect("apply release install");

    let calls = calls.lock().expect("redis provision calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].service_name, "judge-worker");
    assert_eq!(calls[0].resources.len(), release.redis.len());
    assert_eq!(calls[0].resources[0].kind, "consumer-group");
    drop(calls);

    let logs = store.operation_logs("op-release-redis-provision");
    assert!(logs.iter().any(|log| {
        log.step_id == "redis-resources:judge-worker"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("created")
            && log
                .data
                .get("provisioned")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("consumer_group")
                            .and_then(serde_json::Value::as_str)
                            == Some("judge-worker")
                    })
                })
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:judge-worker"
            && log
                .data
                .get("redis_resources")
                .and_then(serde_json::Value::as_str)
                == Some("created")
            && log
                .data
                .get("redis_provision")
                .and_then(|value| value.get("endpoint"))
                .and_then(serde_json::Value::as_str)
                == Some("127.0.0.1:6379")
    }));
}

#[test]
fn tcp_redis_provisioner_creates_judge_stream_and_consumer_group() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local redis test listener");
    let endpoint = listener.local_addr().expect("redis test addr").to_string();
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_thread = Arc::clone(&captured);
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept redis command");
        let mut buffer = [0_u8; 512];
        let bytes = stream.read(&mut buffer).expect("read redis command");
        captured_thread
            .lock()
            .expect("captured redis command")
            .push(
                std::str::from_utf8(&buffer[..bytes])
                    .expect("redis command utf8")
                    .to_string(),
            );
        stream.write_all(b"+OK\r\n").expect("write redis ok");
    });

    let provisioner = TcpRedisResourceProvisioner::new(endpoint);
    let result = provisioner
        .provision_resources(&RedisProvisionRequest {
            service_name: "judge-worker".to_string(),
            resources: vec![ServiceRedisResource {
                service_name: "judge-worker".to_string(),
                name: "redis".to_string(),
                kind: "consumer-group".to_string(),
                usage: "judge task workers".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            }],
        })
        .expect("provision redis resource");
    handle.join().expect("redis listener thread");

    assert_eq!(result.status, "created");
    assert_eq!(result.provisioned[0].stream, "ojos:judge:task");
    assert_eq!(result.provisioned[0].consumer_group, "judge-worker");
    let command = captured.lock().expect("captured redis command").join("\n");
    assert!(command.contains("XGROUP"));
    assert!(command.contains("CREATE"));
    assert!(command.contains("ojos:judge:task"));
    assert!(command.contains("judge-worker"));
    assert!(command.contains("MKSTREAM"));
}

#[test]
fn release_install_provisions_storage_resources_with_runtime_provisioner() {
    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/storage-service/service.yaml"))
            .unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/storage-service/release.yaml"))
            .unwrap();
    let request = ActionRequest::new(
        "op-release-storage-provision",
        "release.install",
        [("service_id".to_string(), "storage-service".to_string())]
            .into_iter()
            .collect(),
    );
    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release-aware operation");
    let confirmed = confirm_operation(&operation).expect("confirm release install");

    let calls = Arc::new(Mutex::new(Vec::new()));
    let storage = RecordingStorageResourceProvisioner {
        calls: Arc::clone(&calls),
    };
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(confirmed).expect("put operation");

    OperationExecutor::with_runtime_provisioners(
        &mut store,
        StaticEndpointProbe,
        DeferredAuthPermissionRegistrar,
        DeferredRedisResourceProvisioner,
        storage,
        DeferredMigrationRunner,
    )
    .apply("op-release-storage-provision")
    .expect("apply release install");

    let calls = calls.lock().expect("storage provision calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].service_name, "storage-service");
    assert_eq!(calls[0].resources.len(), release.storage.len());
    assert!(
        calls[0]
            .resources
            .iter()
            .any(|resource| resource.bucket == "submissions")
    );
    drop(calls);

    let logs = store.operation_logs("op-release-storage-provision");
    assert!(logs.iter().any(|log| {
        log.step_id == "storage-resources:storage-service"
            && log.data.get("status").and_then(serde_json::Value::as_str) == Some("ensured")
            && log
                .data
                .get("provisioned")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("bucket").and_then(serde_json::Value::as_str)
                            == Some("submissions")
                    })
                })
    }));
    assert!(logs.iter().any(|log| {
        log.step_id == "install-pipeline:storage-service"
            && log
                .data
                .get("storage_resources")
                .and_then(serde_json::Value::as_str)
                == Some("ensured")
            && log
                .data
                .get("storage_provision")
                .and_then(|value| value.get("endpoint"))
                .and_then(serde_json::Value::as_str)
                == Some("http://storage-service.test")
    }));
}

#[test]
fn http_storage_provisioner_ensures_declared_buckets() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local storage test listener");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("storage test addr")
    );
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_thread = Arc::clone(&captured);
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept storage request");
        let mut buffer = [0_u8; 1024];
        let bytes = stream.read(&mut buffer).expect("read storage request");
        captured_thread
            .lock()
            .expect("captured storage request")
            .push(
                std::str::from_utf8(&buffer[..bytes])
                    .expect("storage request utf8")
                    .to_string(),
            );
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 41\r\n\r\n{\"bucket\":\"submissions\",\"created\":true}",
            )
            .expect("write storage response");
    });

    let provisioner = HttpStorageResourceProvisioner::new(endpoint);
    let result = provisioner
        .provision_resources(&StorageProvisionRequest {
            service_name: "storage-service".to_string(),
            resources: vec![ServiceStorageResource {
                service_name: "storage-service".to_string(),
                object_type: "submission-code".to_string(),
                bucket: "submissions".to_string(),
                path_prefix: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            }],
        })
        .expect("provision storage resource");
    handle.join().expect("storage listener thread");

    assert_eq!(result.status, "ensured");
    assert_eq!(result.provisioned[0].bucket, "submissions");
    let request = captured
        .lock()
        .expect("captured storage request")
        .join("\n");
    assert!(request.starts_with("PUT /api/storage/buckets/submissions "));
}

#[test]
fn release_create_and_update_register_release_records() {
    assert_eq!(
        capability_for_action("release.create"),
        ActionCapabilityStatus::StoreBacked
    );
    assert_eq!(
        capability_for_action("release.update"),
        ActionCapabilityStatus::StoreBacked
    );

    let root = repo_root();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let create = release_create_operation(
        "op-release-create-record",
        &release,
        Some("local://custom-gateway"),
    )
    .expect("release create operation");
    let update = release_update_operation(
        "op-release-update-record",
        &release,
        Some("local://updated-gateway"),
    )
    .expect("release update operation");

    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(create).expect("put create");
    let created = OperationExecutor::new(&mut store)
        .apply("op-release-create-record")
        .expect("apply create");
    assert_eq!(created.status, OperationStatus::Succeeded);
    assert_eq!(
        store
            .get_service_release("gateway", &release.version)
            .unwrap()
            .unwrap()
            .release_url,
        "local://custom-gateway"
    );

    store.put_operation(update).expect("put update");
    let updated = OperationExecutor::new(&mut store)
        .apply("op-release-update-record")
        .expect("apply update");
    assert_eq!(updated.status, OperationStatus::Succeeded);
    assert_eq!(
        store
            .get_service_release("gateway", &release.version)
            .unwrap()
            .unwrap()
            .release_url,
        "local://updated-gateway"
    );

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-release-update-record")
        .expect("rollback update");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    assert_eq!(
        store
            .get_service_release("gateway", &release.version)
            .unwrap()
            .unwrap()
            .release_url,
        "local://custom-gateway"
    );
}

#[test]
fn release_install_rollback_restores_previous_registry_resources() {
    let root = repo_root();
    let mut old_service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    old_service.version = "0.0.9".to_string();
    let new_service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let new_release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();

    let old_release = ServiceRelease {
        service_name: "gateway".to_string(),
        version: "0.0.9".to_string(),
        release_url: "local://old-gateway".to_string(),
        manifest: serde_json::json!({
            "service_name": "gateway",
            "version": "0.0.9"
        }),
        checksum: "old-checksum".to_string(),
        created_at: String::new(),
    };
    let old_route = ServiceRoute {
        path: "/old-gateway".to_string(),
        method: "GET".to_string(),
        target_type: "endpoint-group".to_string(),
        target_service_name: "gateway".to_string(),
        target_selector: serde_json::json!({ "group": "gateway[*]" }),
        permission: "gateway.old".to_string(),
        enabled: true,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_migration = ServiceMigrationRecord {
        service_name: "gateway".to_string(),
        migration_version: "0000-old".to_string(),
        checksum: "old-migration".to_string(),
        status: "applied".to_string(),
        applied_at: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_permission = ServicePermissionRecord {
        service_name: "gateway".to_string(),
        permission_key: "gateway.old".to_string(),
        source: "release".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_frontend = ServiceFrontendEntry {
        service_name: "gateway".to_string(),
        enabled: true,
        route_prefix: "/old".to_string(),
        remote_entry: "/old/remoteEntry.js".to_string(),
        menu_items: vec!["old.gateway".to_string()],
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_api_surface = ServiceApiSurface {
        service_name: "gateway".to_string(),
        version: "0.0.9".to_string(),
        api_id: "gateway.old.health".to_string(),
        protocol: "http".to_string(),
        port_name: "http".to_string(),
        path_prefix: "/old/health".to_string(),
        methods: vec!["GET".to_string()],
        visibility: "descendants".to_string(),
        auth_mode: "public".to_string(),
        permission: "public".to_string(),
        stability: "stable".to_string(),
        api_version: "v1".to_string(),
        rate_limit: String::new(),
        timeout: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_redis = ServiceRedisResource {
        service_name: "gateway".to_string(),
        name: "old-stream".to_string(),
        kind: "stream".to_string(),
        usage: "old queue".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_storage = ServiceStorageResource {
        service_name: "gateway".to_string(),
        object_type: "old-object".to_string(),
        bucket: "old-bucket".to_string(),
        path_prefix: "/old".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_config = RenderedServiceConfig {
        service_name: "gateway".to_string(),
        version: "0.0.9".to_string(),
        config: serde_json::json!({ "old": true }),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_host_service = HostService {
        host_ip: "127.0.0.1".to_string(),
        service_name: "gateway".to_string(),
        version: "0.0.9".to_string(),
        status: "running".to_string(),
        config: serde_json::json!({ "old": true }),
        labels: serde_json::json!({ "source": "old-release" }),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let old_endpoint = Endpoint {
        endpoint: "127.0.0.1:18080:gateway".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "healthy".to_string(),
        reachable: true,
        display_name: "Old Gateway".to_string(),
        note: String::new(),
        config: serde_json::json!({ "old": true }),
        created_at: String::new(),
        updated_at: String::new(),
    };

    let request = ActionRequest::new(
        "op-release-rollback-restores-registry",
        "release.install",
        [("service_id".to_string(), "gateway".to_string())]
            .into_iter()
            .collect(),
    );
    let operation = plan_action_request_with_releases(
        &request,
        std::slice::from_ref(&new_service),
        std::slice::from_ref(&new_release),
        &[],
        &[],
        None,
    )
    .expect("release-aware operation");
    let confirmed = confirm_operation(&operation).expect("confirm release install");

    let mut store = MemoryOrchestratorStore::new();
    store.put_service(old_service).expect("seed old service");
    store
        .upsert_host_service(old_host_service)
        .expect("seed old host service");
    store.put_endpoint(old_endpoint).expect("seed old endpoint");
    store
        .upsert_service_release(old_release)
        .expect("seed old release");
    store
        .upsert_service_route(old_route)
        .expect("seed old route");
    store
        .upsert_service_migration_record(old_migration)
        .expect("seed old migration");
    store
        .upsert_service_permission_record(old_permission)
        .expect("seed old permission");
    store
        .upsert_service_frontend_entry(old_frontend)
        .expect("seed old frontend");
    store
        .upsert_service_api_surface(old_api_surface)
        .expect("seed old api surface");
    store
        .upsert_service_redis_resource(old_redis)
        .expect("seed old redis");
    store
        .upsert_service_storage_resource(old_storage)
        .expect("seed old storage");
    store
        .upsert_rendered_service_config(old_config)
        .expect("seed old config");
    store.put_operation(confirmed).expect("put operation");

    let applied = OperationExecutor::new(&mut store)
        .apply("op-release-rollback-restores-registry")
        .expect("apply release install");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    assert_eq!(
        store.get_service("gateway").unwrap().unwrap().version,
        new_release.version
    );
    assert_eq!(
        store
            .get_endpoint("127.0.0.1:8080:gateway")
            .unwrap()
            .unwrap()
            .service_id,
        "gateway"
    );
    assert!(
        store
            .get_endpoint("127.0.0.1:18080:gateway")
            .unwrap()
            .is_none(),
        "old endpoint should be replaced during release install"
    );
    assert!(
        store
            .service_routes()
            .iter()
            .any(|route| route.path == "/api/**" && route.target_service_name == "gateway")
    );
    assert!(
        store
            .service_api_surfaces()
            .iter()
            .any(|api| api.service_name == "gateway" && api.api_id == "gateway.health")
    );
    assert!(
        !store
            .service_api_surfaces()
            .iter()
            .any(|api| api.service_name == "gateway" && api.api_id == "gateway.old.health")
    );
    assert!(
        !store
            .service_permission_records()
            .iter()
            .any(|permission| permission.permission_key == "gateway.old")
    );

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-release-rollback-restores-registry")
        .expect("rollback release install");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    assert_eq!(
        store.get_service("gateway").unwrap().unwrap().version,
        "0.0.9"
    );
    assert_eq!(
        store
            .get_host_service("127.0.0.1", "gateway")
            .unwrap()
            .unwrap()
            .version,
        "0.0.9"
    );
    assert!(
        store
            .get_endpoint("127.0.0.1:8080:gateway")
            .unwrap()
            .is_none(),
        "new endpoint should be removed during rollback"
    );
    assert_eq!(
        store
            .get_endpoint("127.0.0.1:18080:gateway")
            .unwrap()
            .unwrap()
            .display_name,
        "Old Gateway"
    );
    assert!(
        store
            .get_service_release("gateway", "0.0.9")
            .unwrap()
            .is_some(),
        "old release should be restored"
    );
    assert!(
        store
            .get_service_release("gateway", &new_release.version)
            .unwrap()
            .is_none(),
        "new release should be removed during rollback"
    );
    assert_eq!(store.service_routes()[0].path, "/old-gateway");
    assert_eq!(store.service_api_surfaces()[0].api_id, "gateway.old.health");
    assert_eq!(
        store.service_migration_records()[0].migration_version,
        "0000-old"
    );
    assert_eq!(
        store.service_permission_records()[0].permission_key,
        "gateway.old"
    );
    assert_eq!(store.service_frontend_entries()[0].route_prefix, "/old");
    assert_eq!(store.service_redis_resources()[0].name, "old-stream");
    assert_eq!(store.service_storage_resources()[0].bucket, "old-bucket");
    assert_eq!(store.rendered_service_configs()[0].version, "0.0.9");
}

#[test]
fn release_delete_is_store_backed_and_rollback_restores_registry() {
    assert_eq!(
        capability_for_action("release.delete"),
        ActionCapabilityStatus::StoreBacked
    );

    let service = valid_service();
    let release = ServiceRelease {
        service_name: service.id.clone(),
        version: service.version.clone(),
        release_url: "local://demo-api".to_string(),
        manifest: serde_json::json!({
            "service_name": service.id,
            "version": service.version
        }),
        checksum: "demo-checksum".to_string(),
        created_at: String::new(),
    };
    let route = ServiceRoute {
        path: "/demo".to_string(),
        method: "GET".to_string(),
        target_type: "endpoint-group".to_string(),
        target_service_name: service.id.clone(),
        target_selector: serde_json::json!({ "group": "demo-api[*]" }),
        permission: "demo.read".to_string(),
        enabled: true,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let operation =
        release_delete_operation("op-release-delete", &service.id, Some(&service.version))
            .expect("release delete operation");
    let confirmed = confirm_operation(&operation).expect("confirm release delete");

    let mut store = MemoryOrchestratorStore::new();
    store.put_service(service.clone()).expect("seed service");
    store.upsert_service_release(release).expect("seed release");
    store.upsert_service_route(route).expect("seed route");
    store.put_operation(confirmed).expect("put operation");

    let deleted = OperationExecutor::new(&mut store)
        .apply("op-release-delete")
        .expect("apply release delete");
    assert_eq!(deleted.status, OperationStatus::Succeeded);
    assert!(
        store
            .get_service_release(&service.id, &service.version)
            .unwrap()
            .is_none()
    );
    assert!(store.service_routes().is_empty());

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-release-delete")
        .expect("rollback release delete");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    assert!(
        store
            .get_service_release(&service.id, &service.version)
            .unwrap()
            .is_some()
    );
    assert_eq!(store.service_routes()[0].path, "/demo");
}

#[test]
fn release_rollback_dispatches_to_release_install_rollback() {
    assert_eq!(
        capability_for_action("release.rollback"),
        ActionCapabilityStatus::StoreBacked
    );

    let root = repo_root();
    let service =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let release =
        validate_service_release_file(&root, Path::new("services/gateway/release.yaml")).unwrap();
    let install_request = ActionRequest::new(
        "op-release-install-for-release-rollback",
        "release.install",
        [("service_id".to_string(), "gateway".to_string())]
            .into_iter()
            .collect(),
    );
    let install = plan_action_request_with_releases(
        &install_request,
        std::slice::from_ref(&service),
        std::slice::from_ref(&release),
        &[],
        &[],
        None,
    )
    .expect("release install operation");
    let install = confirm_operation(&install).expect("confirm install");

    let rollback = release_rollback_operation(
        "op-release-rollback-action",
        "gateway",
        Some(&release.version),
        Some("op-release-install-for-release-rollback"),
    )
    .expect("release rollback operation");
    let rollback = confirm_operation(&rollback).expect("confirm release rollback");

    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(install).expect("put install");
    OperationExecutor::new(&mut store)
        .apply("op-release-install-for-release-rollback")
        .expect("apply install");
    assert!(
        store
            .get_service_release("gateway", &release.version)
            .unwrap()
            .is_some()
    );
    store.put_operation(rollback).expect("put rollback");

    let rolled_back = OperationExecutor::new(&mut store)
        .apply("op-release-rollback-action")
        .expect("apply release rollback");
    assert_eq!(rolled_back.status, OperationStatus::Succeeded);
    assert!(
        store
            .get_service_release("gateway", &release.version)
            .unwrap()
            .is_none()
    );
    let target = store
        .get_operation("op-release-install-for-release-rollback")
        .unwrap()
        .unwrap();
    assert_eq!(target.status, OperationStatus::RolledBack);
}

#[test]
fn core_plans_service_endpoint_link_and_topology_operations() {
    let lifecycle =
        service_lifecycle_operation("op-service-restart", "service.restart", "judge-worker")
            .expect("service restart operation");
    assert_eq!(lifecycle.action, "service.restart");
    assert_eq!(lifecycle.target_type, "Service");
    assert_eq!(lifecycle.target_id, "judge-worker");

    let endpoints = vec![
        Endpoint {
            endpoint: "192.168.1.10:8080:gateway".to_string(),
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
            endpoint: "192.168.1.10:8082:judge-api".to_string(),
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
    let endpoint_operation = endpoint_create_operation("op-endpoint-register", &endpoints[0])
        .expect("endpoint create operation");
    assert_eq!(endpoint_operation.action, "endpoint.create");
    assert_eq!(endpoint_operation.target_type, "Endpoint");
    assert_eq!(endpoint_operation.target_id, "192.168.1.10:8080:gateway");
    let endpoint_update =
        endpoint_update_operation("op-endpoint-update", &endpoints[0]).expect("endpoint update");
    assert_eq!(endpoint_update.action, "endpoint.update");
    let endpoint_delete =
        endpoint_delete_operation("op-endpoint-delete", "192.168.1.10:8080:gateway")
            .expect("endpoint delete");
    assert_eq!(endpoint_delete.action, "endpoint.delete");
    let endpoint_health =
        endpoint_health_check_operation("op-endpoint-health", "192.168.1.10:8080:gateway")
            .expect("endpoint health");
    assert_eq!(endpoint_health.action, "endpoint.health.check");

    let link = Link {
        source_endpoint: "192.168.1.10:8080:gateway".to_string(),
        target_endpoint: "192.168.1.10:8082:judge-api".to_string(),
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
        "192.168.1.10:8080:gateway -> 192.168.1.10:8082:judge-api"
    );
    let link_update =
        link_update_operation("op-link-update", &link, &endpoints).expect("link update");
    assert_eq!(link_update.action, "link.update");
    let link_delete = link_delete_operation("op-link-delete", &link).expect("link delete");
    assert_eq!(link_delete.action, "link.delete");
    let link_health = link_health_check_operation("op-link-health", &link).expect("link health");
    assert_eq!(link_health.action, "link.health.check");

    let service_health = service_health_check_operation(
        "op-service-health",
        "judge-api",
        Some("192.168.1.10:8082:judge-api"),
    )
    .expect("service health operation");
    assert_eq!(service_health.action, "service.health.check");
    let wrong_service_health = service_health_check_operation(
        "op-service-health-wrong-endpoint",
        "gateway",
        Some("192.168.1.10:8082:judge-api"),
    )
    .expect_err("service health endpoint third segment must match service_id");
    assert!(
        wrong_service_health
            .to_string()
            .contains("service name must match service_id")
    );
    let service_logs = log_create_operation(
        "op-service-logs",
        "judge-api",
        Some("192.168.1.10:8082:judge-api"),
    )
    .expect("service logs operation");
    assert_eq!(service_logs.action, "log.create");
    let wrong_service_logs = log_create_operation(
        "op-service-logs-wrong-endpoint",
        "gateway",
        Some("192.168.1.10:8082:judge-api"),
    )
    .expect_err("log endpoint third segment must match service_id");
    assert!(
        wrong_service_logs
            .to_string()
            .contains("service name must match service_id")
    );
    let operation_logs = log_query_operation("op-operation-logs", "op-service-logs")
        .expect("operation logs operation");
    assert_eq!(operation_logs.action, "log.query");
    assert_eq!(operation_logs.target_id, "op-service-logs");
    let diagnostics_export =
        diagnostic_export_operation("op-diag-export", "diag-sample", "markdown")
            .expect("diagnostics export operation");
    assert_eq!(diagnostics_export.action, "diagnostic.export");
    assert_eq!(diagnostics_export.target_id, "diag-sample");

    let topology = build_topology(
        "192.168.1.10:8080:gateway".to_string(),
        vec!["gateway".to_string(), "judge-api".to_string()],
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
    assert_eq!(topology_operation.target_id, "192.168.1.10:8080:gateway");
}

#[test]
fn action_registry_contains_required_actions_and_no_forbidden_actions() {
    let root = repo_root();
    let shared_schemas = load_shared_schemas(&root).expect("shared schemas should load");
    ensure_shared_schemas_loaded(&shared_schemas).expect("shared schemas should be complete");
    let text = fs::read_to_string(root.join("platform/schemas/orchestrator/actions.yaml")).unwrap();
    let value: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    let actions = action_set(&root);
    assert!(
        value.get("forbidden_prefixes").is_none(),
        "actions.yaml should only define shared GUI/TUI actions"
    );

    for prefix in FORMAL_ACTION_PREFIXES {
        for verb in ["create", "list", "get", "update", "delete"] {
            let required = format!("{prefix}{verb}");
            assert!(
                actions.contains(required.as_str()),
                "missing action {required}"
            );
        }
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
    assert_eq!(descriptors.len(), schema_actions.len());

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
        action_descriptor("release.install").map(|item| item.target_type),
        Some("ServiceRelease")
    );
    assert_eq!(
        action_descriptor("release.install").map(|item| item.plan_mode),
        Some(ActionPlanMode::ConfirmedPlan)
    );
    assert_eq!(
        action_descriptor("topology.create").map(|item| item.target_type),
        Some("Topology"),
        "topology action is mapped to Topology and does not introduce a new core object"
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
        "services/problem-service/service.yaml",
    ]
    .into_iter()
    .map(|path| validate_service_manifest_file(&root, Path::new(path)).unwrap())
    .collect::<Vec<_>>();
    let set =
        validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
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
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
        Endpoint {
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
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
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
        endpoints.clone(),
        vec![Link {
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8083:problem-service".to_string(),
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
        "release.install",
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
    .expect("release install preview");
    assert_eq!(install_preview.target_type, "ServiceRelease");
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
            (
                "source_endpoint".to_string(),
                "127.0.0.1:8080:gateway".to_string(),
            ),
            (
                "target_endpoint".to_string(),
                "127.0.0.1:8083:problem-service".to_string(),
            ),
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
fn planner_validates_log_create_endpoint_service_identity() {
    let services = vec![valid_service()];
    let endpoints = vec![Endpoint {
        endpoint: "127.0.0.1:18080:demo-api".to_string(),
        service_id: "demo-api".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "ok".to_string(),
        reachable: true,
        display_name: "Demo API".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }];

    let ok_request = ActionRequest::new(
        "op-plan-log-create",
        "log.create",
        [
            ("service_id".to_string(), "demo-api".to_string()),
            (
                "endpoint".to_string(),
                "127.0.0.1:18080:demo-api".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let operation = plan_action_request(&ok_request, &services, &[], &endpoints, None)
        .expect("log.create plan should validate matching endpoint identity");
    assert_eq!(operation.action, "log.create");
    assert_eq!(operation.target_type, "LogView");

    let wrong_request = ActionRequest::new(
        "op-plan-log-create-wrong-service",
        "log.create",
        [
            ("service_id".to_string(), "demo-api".to_string()),
            (
                "endpoint".to_string(),
                "127.0.0.1:18081:other-service".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let err = plan_action_request(&wrong_request, &services, &[], &endpoints, None)
        .expect_err("log.create plan must reject endpoint/service mismatch");
    assert!(
        err.to_string()
            .contains("service name must match service_id")
    );
}

#[test]
fn planner_rejects_two_part_endpoint_fields_before_generic_actions() {
    let request = ActionRequest::new(
        "op-plan-secret-distribute",
        "secret.distribute",
        [
            (
                "secret_id".to_string(),
                "secret://demo-api/default".to_string(),
            ),
            ("endpoint".to_string(), "127.0.0.1:18080".to_string()),
            ("service_id".to_string(), "demo-api".to_string()),
        ]
        .into_iter()
        .collect(),
    );

    let err = plan_action_request(&request, &[valid_service()], &[], &[], None)
        .expect_err("generic actions must still reject two-part runtime endpoints");
    assert!(
        err.to_string()
            .contains("endpoint must be ip:port:service-name")
    );
}

#[test]
fn operation_workbench_builds_preview_for_every_action() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    let descriptors = validate_action_catalog(&schemas).expect("action catalog");
    let services = [
        "services/gateway/service.yaml",
        "services/problem-service/service.yaml",
    ]
    .into_iter()
    .map(|path| validate_service_manifest_file(&root, Path::new(path)).unwrap())
    .collect::<Vec<_>>();
    let releases = [
        "services/gateway/release.yaml",
        "services/problem-service/release.yaml",
    ]
    .into_iter()
    .map(|path| validate_service_release_file(&root, Path::new(path)).unwrap())
    .collect::<Vec<_>>();
    let set =
        validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
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
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
        Endpoint {
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
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
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
        endpoints.clone(),
        vec![Link {
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8083:problem-service".to_string(),
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
        let workbench = build_operation_workbench_with_releases(
            descriptor.action,
            &schemas,
            &services,
            &releases,
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
    let set =
        validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
    let endpoints = vec![Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
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
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string()],
        endpoints.clone(),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let workbench = build_operation_workbench(
        "release.install",
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
        validate_service_manifest_file(&root, Path::new("services/problem-service/service.yaml"))
            .unwrap(),
    ];
    let set =
        validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml")).unwrap();
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
            config: serde_json::json!({}),
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
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        },
    ];
    let topology = build_topology(
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
        endpoints.clone(),
        vec![Link {
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8081:problem-service".to_string(),
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
        "release.install",
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
        "problem-service",
        &schemas,
        &services,
        std::slice::from_ref(&set),
        &endpoints,
        Some(&topology),
    )
    .expect("updated workbench");
    assert_eq!(
        updated.workbench.request.field("service_id"),
        Some("problem-service")
    );
    assert_eq!(updated.workbench.preview.target_id, "problem-service");

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
    let context = crate::workbench::load_operation_workbench_context_with_database_url(&root, None)
        .expect("workbench context")
        .with_memory_store();

    let descriptors = validate_action_catalog(&context.schemas).expect("action catalog");
    assert_eq!(context.schemas.action_count(), descriptors.len());
    assert_eq!(context.services.len(), 12);
    assert_eq!(context.releases.len(), 12);
    assert_eq!(context.templates.len(), 5);
    assert!(!context.endpoints.is_empty());
    assert!(!context.links.is_empty());
    assert!(context.topology.is_some());

    for descriptor in descriptors {
        context
            .build_session(descriptor.action)
            .unwrap_or_else(|err| panic!("{} should build from context: {err}", descriptor.action));
    }

    let session = context
        .build_session("release.install")
        .expect("release install session");
    assert_eq!(
        session
            .current_operation
            .request
            .get("release_manifest")
            .and_then(|value| value.get("service_name"))
            .and_then(serde_json::Value::as_str),
        Some("gateway")
    );
    let updated = context
        .update_field(&session, "service_id", "problem-service")
        .expect("field update should rebuild through core context");
    assert_eq!(
        updated.workbench.request.field("service_id"),
        Some("problem-service")
    );
    assert_eq!(updated.workbench.preview.target_id, "problem-service");
    assert_eq!(
        updated
            .current_operation
            .request
            .get("release_manifest")
            .and_then(|value| value.get("service_name"))
            .and_then(serde_json::Value::as_str),
        Some("problem-service")
    );

    let service_field = updated
        .workbench
        .form_fields
        .iter()
        .find(|field| field.name == "service_id")
        .expect("service_id field");
    let suggested = context.suggested_field_values(service_field);
    assert!(suggested.contains(&"gateway".to_string()));
    assert!(suggested.contains(&"problem-service".to_string()));

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
fn operation_workbench_context_can_load_from_store_state() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("schemas");
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    gateway.kind = "gateway".to_string();
    gateway.endpoint.default_port = 8080;
    let mut problem_api = valid_service();
    problem_api.id = "problem-service".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    let gateway_release = ServiceReleaseManifest {
        schema_version: 1,
        service_name: "gateway".to_string(),
        version: "0.1.0".to_string(),
        description: "Gateway release".to_string(),
        service_type: "gateway".to_string(),
        source: ReleaseSourceDecl {
            kind: "local".to_string(),
            url: "local://services/gateway".to_string(),
            checksum: String::new(),
        },
        runtime: ReleaseRuntimeDecl {
            kind: "image".to_string(),
            image: String::new(),
            binary: String::new(),
            system_service: String::new(),
            command: String::new(),
            args: Vec::new(),
            working_dir: String::new(),
            env: BTreeMap::new(),
        },
        frontend: ReleaseFrontendDecl::default(),
        backend: ReleaseBackendDecl {
            protocol: "http".to_string(),
            port: 8080,
            health_path: "/health".to_string(),
        },
        migrations: Vec::new(),
        apis: Vec::new(),
        permissions: vec!["demo.read".to_string()],
        routes: vec![ReleaseRouteDecl {
            path: "/api/gateway/**".to_string(),
            method: "ANY".to_string(),
            target_type: "endpoint-group".to_string(),
            target: "gateway[*]".to_string(),
            permission: "public".to_string(),
        }],
        redis: Vec::new(),
        storage: Vec::new(),
        dependencies: Vec::new(),
        required_apis: Vec::new(),
        service_identity: ReleaseServiceIdentityDecl::default(),
        config_schema: serde_json::json!({}),
        secrets: Vec::new(),
        observability: ReleaseObservabilityDecl::default(),
    };
    store
        .upsert_service_release(ServiceRelease {
            service_name: "gateway".to_string(),
            version: "0.1.0".to_string(),
            release_url: "local://services/gateway".to_string(),
            manifest: serde_json::to_value(&gateway_release).expect("release json"),
            checksum: String::new(),
            created_at: String::new(),
        })
        .expect("put gateway release");
    store
        .put_endpoint(Endpoint {
            endpoint: "10.0.0.10:8080:gateway".to_string(),
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
        .expect("put gateway endpoint");
    store
        .put_endpoint(Endpoint {
            endpoint: "10.0.0.10:8081:problem-service".to_string(),
            service_id: "problem-service".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
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
            source_endpoint: "10.0.0.10:8080:gateway".to_string(),
            target_endpoint: "10.0.0.10:8081:problem-service".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            health: "healthy".to_string(),
            latency_ms: Some(2),
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put link");
    let topology = store.build_topology_view().expect("topology");
    store.put_topology(topology).expect("put topology");

    let context = load_operation_workbench_context_from_store(schemas, &store)
        .expect("store-backed workbench context");
    assert!(
        !context.uses_persistent_store(),
        "direct Store-backed contexts do not assume ORCHESTRATOR_DATABASE_URL persistence"
    );
    assert_eq!(context.services.len(), 2);
    assert_eq!(context.releases.len(), 1);
    let release_session = context
        .build_session("release.install")
        .expect("release install session from store context");
    assert_eq!(
        release_session
            .current_operation
            .request
            .get("release_manifest")
            .and_then(|value| value.get("service_name"))
            .and_then(serde_json::Value::as_str),
        Some("gateway")
    );
    assert_eq!(context.endpoints[0].endpoint, "10.0.0.10:8080:gateway");
    assert_eq!(context.links.len(), 1);
    assert!(context.topology.is_some());

    let session = context
        .build_session("link.health.check")
        .expect("link health session from store context");
    assert_eq!(
        session.workbench.request.field("source_endpoint"),
        Some("127.0.0.1:8080:gateway"),
        "default request remains schema-driven before fields are changed"
    );
    let updated = context
        .update_field(&session, "source_endpoint", "10.0.0.10:8080:gateway")
        .and_then(|session| {
            context.update_field(
                &session,
                "target_endpoint",
                "10.0.0.10:8081:problem-service",
            )
        })
        .expect("store endpoint fields should be accepted");
    assert_eq!(
        updated.workbench.preview.target_id,
        "10.0.0.10:8080:gateway -> 10.0.0.10:8081:problem-service"
    );
}

#[test]
fn operation_workbench_context_applies_store_backed_core_actions() {
    let root = repo_root();
    let context = crate::workbench::load_operation_workbench_context_with_database_url(&root, None)
        .expect("workbench context")
        .with_memory_store();

    let endpoint_session = context
        .build_session("endpoint.create")
        .expect("endpoint create session");
    let endpoint_applied = context
        .apply(&endpoint_session)
        .expect("endpoint create should use context store");
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

    assert!(
        context.build_session("set.apply").is_err(),
        "service-name endpoint groups are derived queries, not formal actions"
    );
}

#[test]
fn orchestrator_entrypoints_require_reachable_persistent_store_when_database_url_is_set() {
    let root = repo_root();
    let unavailable_url =
        Some("postgres://postgres:local@127.0.0.1:1/ojos_orchestrator".to_string());
    let context_error = crate::workbench::load_operation_workbench_context_with_database_url(
        &root,
        unavailable_url.clone(),
    )
    .expect_err("persistent workbench context must not fall back to repo state");
    assert!(
        context_error
            .to_string()
            .contains("ORCHESTRATOR_DATABASE_URL store unavailable")
    );
    let view_error =
        crate::view::load_orchestrator_view_with_database_url(&root, unavailable_url.clone())
            .expect_err("persistent view must not fall back to repo state");
    assert!(
        view_error
            .to_string()
            .contains("ORCHESTRATOR_DATABASE_URL store unavailable")
    );
    let console_error = OrchestratorActionConsole::load_with_database_url(root, unavailable_url)
        .expect_err("persistent console must not fall back to repo or memory state");
    assert!(
        console_error
            .to_string()
            .contains("ORCHESTRATOR_DATABASE_URL store unavailable")
    );
}

#[test]
fn repo_manifest_registry_sync_seeds_only_services_and_releases() {
    let root = repo_root();
    let context = crate::workbench::load_operation_workbench_context_with_database_url(&root, None)
        .expect("workbench context")
        .with_memory_store();
    let mut store = MemoryOrchestratorStore::new();

    crate::dispatcher::sync_repo_manifest_registry_to_store(&mut store, &context)
        .expect("sync repo manifest registry");

    assert!(
        store
            .get_service("storage-service")
            .expect("get storage service")
            .is_some(),
        "storage-service service.yaml should be synced into registry"
    );
    assert!(
        store
            .service_releases()
            .iter()
            .any(|release| release.service_name == "storage-service"),
        "storage-service release.yaml should be synced into registry"
    );
    assert!(
        store.host_services().is_empty(),
        "manifest registry sync must not create runtime HostService records"
    );
    assert!(
        store.endpoints().is_empty(),
        "manifest registry sync must not create runtime Endpoint records"
    );
    assert!(
        store.service_api_surfaces().is_empty(),
        "manifest registry sync must not register API surfaces before release.install"
    );
}

#[test]
fn memory_cache_node_load_is_parent_first() {
    let mut store = MemoryOrchestratorStore::new();
    let child = NodeRecord {
        node_id: "child-node".to_string(),
        host_ip: "127.0.0.2".to_string(),
        parent_node_id: "root-node".to_string(),
        role: "node".to_string(),
        labels: serde_json::json!({}),
        status: "running".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let root = NodeRecord {
        node_id: "root-node".to_string(),
        host_ip: "127.0.0.1".to_string(),
        parent_node_id: String::new(),
        role: "root".to_string(),
        labels: serde_json::json!({}),
        status: "running".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };

    crate::dispatcher::upsert_nodes_parent_first(&mut store, vec![child, root])
        .expect("nodes should load parent-first even when input is child-first");

    assert!(store.get_node("root-node").expect("root node").is_some());
    assert!(store.get_node("child-node").expect("child node").is_some());
    assert_eq!(
        store
            .ancestors_of("child-node")
            .expect("child ancestors")
            .first()
            .map(|node| node.node_id.as_str()),
        Some("root-node")
    );
}

#[test]
fn operation_workbench_session_seed_persists_planned_and_confirmed_state() {
    let root = repo_root();
    let context = crate::workbench::load_operation_workbench_context_with_database_url(&root, None)
        .expect("workbench context")
        .with_memory_store();
    let session = context
        .build_session("release.install")
        .expect("release install session");
    let mut store = MemoryOrchestratorStore::new();

    crate::workbench::seed_session_store(&mut store, &context, &session)
        .expect("seed planned session");
    let planned = store
        .operation(&session.current_operation.operation_id)
        .expect("planned operation should be persisted");
    assert_eq!(planned.status, OperationStatus::Planned);
    assert!(!planned.operation_id.is_empty());
    assert!(!planned.action.is_empty());
    assert!(
        planned
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|steps| !steps.is_empty()),
        "planned operation should persist executable plan steps"
    );

    let confirmed = context.confirm(&session).expect("confirm session");
    crate::workbench::seed_session_store(&mut store, &context, &confirmed)
        .expect("seed confirmed session");
    let confirmed_operation = store
        .operation(&confirmed.current_operation.operation_id)
        .expect("confirmed operation should be persisted");
    assert_eq!(
        confirmed_operation.status,
        OperationStatus::AwaitingConfirmation
    );
    assert!(!confirmed_operation.confirmed_at.is_empty());
}

#[test]
fn shared_schemas_cover_gui_tui_contract() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("shared schemas should load");
    ensure_shared_schemas_loaded(&schemas).expect("shared schemas should be usable");

    assert_eq!(schemas.actions.len(), ACTION_CATALOG.len());
    assert_eq!(schemas.actions.len(), schemas.form_actions.len());
    assert_eq!(schemas.actions.len(), schemas.forms.len());
    assert!(
        schemas
            .form_for("release.install")
            .is_some_and(
                |form| form.fields.iter().any(|field| field.name == "service_id"
                    && field.field_type == "service_id"
                    && field.required)
            ),
        "release.install should expose required service_id form field"
    );
    assert!(
        schemas.form_for("topology.get").is_some_and(|form| {
            form.fields
                .iter()
                .all(|field| field.name == "topology_snapshot_id" && !field.required)
        }),
        "topology.get should only expose an optional topology_snapshot_id"
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
            "Template",
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
            "docs/deployment-checklist.md",
            "docs/evidence/production-readiness.json",
            "docs/evidence/release-candidate.json",
            "docs/evidence/staging-drill.md",
            "docs/ops-runbook.md",
            "docs/orchestrator/action-model.md",
            "docs/orchestrator/boundary.md",
            "docs/orchestrator/database.md",
            "docs/orchestrator/gui-tui-parity.md",
            "docs/orchestrator/operation-model.md",
            "docs/orchestrator/requirements.md",
            "docs/orchestrator/topology-model.md",
            "docs/production-readiness.md",
            "docs/release-candidate.md",
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

    let allowed_ops_scripts = [
        "deploy/compose/minio-init.sh",
        "deploy/ops/backup.sh",
        "deploy/ops/alert-firing-drill.sh",
        "deploy/ops/basic-load-soak.sh",
        "deploy/ops/ci-policy.sh",
        "deploy/ops/manager-smoke.sh",
        "deploy/ops/preflight.sh",
        "deploy/ops/redis-recovery-drill.sh",
        "deploy/ops/restore.sh",
        "deploy/ops/rollback-drill.sh",
        "deploy/ops/secret-check.sh",
        "deploy/ops/service-credential-drill.sh",
        "deploy/ops/staging-drill.sh",
        "deploy/ops/trace-e2e-drill.sh",
    ];
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
                && !allowed_ops_scripts.contains(&path.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        script_files,
        Vec::<String>::new(),
        "unexpected script files should not be retained as product or acceptance surface"
    );
}

#[test]
fn form_registry_covers_every_action() {
    let root = repo_root();
    let forms_text =
        fs::read_to_string(root.join("platform/schemas/orchestrator/forms.yaml")).unwrap();
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
    let forms_text =
        fs::read_to_string(root.join("platform/schemas/orchestrator/forms.yaml")).unwrap();
    let plans_text =
        fs::read_to_string(root.join("platform/schemas/orchestrator/plans.yaml")).unwrap();
    let results_text =
        fs::read_to_string(root.join("platform/schemas/orchestrator/results.yaml")).unwrap();
    let errors_text =
        fs::read_to_string(root.join("platform/schemas/orchestrator/errors.yaml")).unwrap();
    let forms: serde_yaml::Value = serde_yaml::from_str(&forms_text).unwrap();
    let plans: serde_yaml::Value = serde_yaml::from_str(&plans_text).unwrap();
    let results: serde_yaml::Value = serde_yaml::from_str(&results_text).unwrap();
    let errors: serde_yaml::Value = serde_yaml::from_str(&errors_text).unwrap();
    let core_objects = [
        "Service",
        "Endpoint",
        "Link",
        "Operation",
        "Topology",
        "LogView",
        "DiagnosticReport",
    ];

    for action in ["operation.create", "diagnostic.create"] {
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
    let view =
        load_orchestrator_view_with_database_url(&root, None).expect("load orchestrator view");
    ensure_view_is_loaded(&view).expect("view should contain core data");

    assert_eq!(view.services.len(), 12);
    assert_eq!(view.schemas.action_count(), view.operations.len());
    assert_eq!(view.schemas.action_count(), view.schemas.form_count());
    assert_eq!(view.templates.len(), 5);
    assert!(!view.endpoints.is_empty());
    assert!(!view.links.is_empty());
    assert!(!view.operations.is_empty());
    assert!(!view.logs.is_empty());
    assert!(
        view.release_registry.iter().any(|row| {
            row.service_name == "gateway"
                && row.record_type == "route"
                && row.name.starts_with("/api/")
                && row.source == "release.yaml"
        }),
        "repo view should expose release.yaml-backed route registry rows"
    );
    assert!(
        view.release_registry
            .iter()
            .any(|row| row.record_type == "redis" && row.source == "release.yaml"),
        "repo view should expose release.yaml-backed redis registry rows"
    );
    let workbench = view
        .operation_workbench
        .as_ref()
        .expect("view should expose shared operation workbench");
    assert_eq!(workbench.selected_action, "release.install");
    assert_eq!(workbench.target, "ServiceRelease gateway");
    assert!(workbench.fields.contains("service_id*"));
    assert!(!workbench.preview_steps.is_empty());
    assert!(
        view.operations
            .iter()
            .any(|item| item.action == "release.install"
                && item.target == "ServiceRelease"
                && !item.risk.is_empty()
                && !item.mode.is_empty()
                && !item.plan_required.is_empty()
                && item.fields.contains("service_id*")),
        "view should expose core action catalog semantics"
    );
    assert!(
        view.operations
            .iter()
            .any(|item| item.action == "topology.create"
                && item.target == "Topology"
                && !item.summary.is_empty()),
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
            .unwrap_or_else(|err| panic!("view endpoint should be ip:port:service-name: {err}"));
    }
    for link in &view.links {
        validate_endpoint_id(&link.from)
            .unwrap_or_else(|err| panic!("view link source should be ip:port:service-name: {err}"));
        validate_endpoint_id(&link.to)
            .unwrap_or_else(|err| panic!("view link target should be ip:port:service-name: {err}"));
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
            endpoint: "192.168.1.10:8080:gateway".to_string(),
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
            endpoint: "192.168.1.10:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
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
    let authority = topology_authority("192.168.1.10:8080:gateway")
        .expect("root authority should derive from endpoint");
    let topology = Topology {
        root_host: authority.root_host.clone(),
        root_endpoint: "192.168.1.10:8080:gateway".to_string(),
        authority,
        services: vec!["gateway".to_string(), "problem-service".to_string()],
        endpoints,
        links: vec![Link {
            source_endpoint: "192.168.1.10:8080:gateway".to_string(),
            target_endpoint: "192.168.1.10:8083:problem-service".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "oj".to_string(),
            health: "unknown".to_string(),
            latency_ms: None,
            config_ref: String::new(),
            secret_ref: "secret://gateway/problem-service".to_string(),
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
            endpoint: "192.168.1.10:8080:gateway".to_string(),
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
                message: "Endpoint and Link are auditable".to_string(),
                redacted: false,
            }],
            created_at: String::new(),
        }],
    };

    validate_topology(&topology).expect("topology should use Endpoint as runtime identity");
}

#[test]
fn topology_rejects_root_host_that_does_not_match_root_endpoint() {
    let authority = topology_authority("192.168.1.10:8080:gateway")
        .expect("root authority should derive from endpoint");
    let topology = Topology {
        root_host: "192.168.1.20".to_string(),
        root_endpoint: "192.168.1.10:8080:gateway".to_string(),
        authority,
        services: vec!["gateway".to_string()],
        endpoints: vec![Endpoint {
            endpoint: "192.168.1.10:8080:gateway".to_string(),
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
            endpoint: "192.168.1.10:8080:gateway".to_string(),
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
            endpoint: "192.168.1.10:8082:judge-api".to_string(),
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
        source_endpoint: "192.168.1.10:8080:gateway".to_string(),
        target_endpoint: "192.168.1.10:8082:judge-api".to_string(),
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
        "192.168.1.10:8080:gateway".to_string(),
        vec!["gateway".to_string(), "judge-api".to_string()],
        endpoints,
        links,
        vec![],
        vec![],
        vec![],
    )
    .expect("topology builder should validate endpoint/link identity");

    assert_eq!(topology.root_endpoint, "192.168.1.10:8080:gateway");
    assert_eq!(topology.root_host, "192.168.1.10");
    assert_eq!(topology.authority.root_endpoint, topology.root_endpoint);
    assert_eq!(topology.authority.root_host, topology.root_host);
    assert_eq!(topology.links.len(), 1);
}

#[test]
fn topology_rejects_unknown_link_endpoint() {
    let endpoints = vec![Endpoint {
        endpoint: "192.168.1.10:8080:gateway".to_string(),
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
        source_endpoint: "192.168.1.10:8080:gateway".to_string(),
        target_endpoint: "192.168.1.20:9101:problem-service".to_string(),
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
        endpoint: "192.168.1.10:8080:gateway".to_string(),
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
        endpoint: "192.168.1.10:8082:judge-api".to_string(),
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
            source_endpoint: "192.168.1.10:8080:gateway".to_string(),
            target_endpoint: "192.168.1.10:8082:judge-api".to_string(),
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

    let err = OperationExecutor::new(&mut store)
        .apply("op-apply-1")
        .expect_err("default executor must not report plan-only lifecycle as success");
    assert_eq!(
        store.operation("op-apply-1").map(|item| &item.status),
        Some(&OperationStatus::Failed)
    );
    assert!(err.to_string().contains("execution is not enabled"));
    assert!(
        store
            .operation_logs("op-apply-1")
            .iter()
            .any(|record| !record.step_id.is_empty()),
        "apply should write step logs"
    );
    assert!(
        store
            .operation_logs("op-apply-1")
            .iter()
            .any(|record| record.step_id == "driver:service.restart"
                && record
                    .data
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    == Some("PLANNED")),
        "plan-only lifecycle should still record fixed driver command details"
    );
}

#[test]
fn operation_plan_is_persisted_in_store() {
    let mut store = MemoryOrchestratorStore::new();
    let operation = plan_operation(
        "op-plan-persisted",
        "operation.create",
        "Operation",
        "gateway",
        serde_json::json!({"action": "release.install", "target_id": "gateway"}),
        serde_json::json!({"steps": [{"action": "plan", "target": "gateway"}]}),
        serde_json::json!({"steps": []}),
    )
    .expect("plan operation");
    store
        .put_operation(operation)
        .expect("persist planned operation");

    let persisted = store
        .get_operation("op-plan-persisted")
        .expect("get operation")
        .expect("operation");
    assert_eq!(persisted.status, OperationStatus::Planned);
    assert_eq!(persisted.created_at, "planned");
    assert_eq!(persisted.updated_at, "planned");
    assert_eq!(persisted.action, "operation.create");
    assert!(
        persisted
            .plan
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|steps| !steps.is_empty())
    );
}

#[test]
fn operation_confirm_updates_store() {
    let mut store = MemoryOrchestratorStore::new();
    let operation = plan_operation(
        "op-confirm-store",
        "release.install",
        "Service",
        "gateway",
        serde_json::json!({"service_id": "gateway"}),
        serde_json::json!({"steps": [{"action": "install", "target": "gateway"}]}),
        serde_json::json!({"steps": [{"action": "remove_service", "target": "gateway"}]}),
    )
    .expect("plan operation");
    store.put_operation(operation).expect("put operation");

    let confirmed = confirm_operation(
        &store
            .get_operation("op-confirm-store")
            .expect("get operation")
            .expect("operation"),
    )
    .expect("confirm operation");
    store
        .put_operation(confirmed)
        .expect("persist confirmed operation");

    let persisted = store
        .get_operation("op-confirm-store")
        .expect("get operation")
        .expect("operation");
    assert_eq!(persisted.status, OperationStatus::AwaitingConfirmation);
    assert_eq!(persisted.confirmed_at, "confirmed");
    assert_eq!(persisted.updated_at, "confirmed");
}

#[test]
fn operation_apply_writes_status_and_logs() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway.clone()).expect("put gateway");
    let operation = confirm_operation(
        &release_install_operation("op-apply-store", &gateway, &[])
            .expect("release install operation"),
    )
    .expect("confirm operation");
    store.put_operation(operation).expect("put operation");

    let applied = OperationExecutor::new(&mut store)
        .apply("op-apply-store")
        .expect("apply operation");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    assert_eq!(applied.started_at, "started");
    assert_eq!(applied.finished_at, "finished");
    assert_eq!(applied.updated_at, "finished");
    assert!(
        store
            .list_operation_logs("op-apply-store")
            .expect("operation logs")
            .iter()
            .any(|record| record.level == "info"
                && !record.created_at.is_empty()
                && record.message.contains("succeeded"))
    );
}

#[test]
fn operation_apply_failure_writes_error_message() {
    let mut store = MemoryOrchestratorStore::new();
    let operation = confirm_operation(
        &service_lifecycle_operation("op-apply-failure-store", "service.start", "missing-service")
            .expect("lifecycle operation"),
    )
    .expect("confirm operation");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::new(&mut store)
        .apply("op-apply-failure-store")
        .expect_err("missing service should fail apply");
    let failed = store
        .get_operation("op-apply-failure-store")
        .expect("get operation")
        .expect("operation");
    assert_eq!(failed.status, OperationStatus::Failed);
    assert_eq!(failed.updated_at, "failed");
    assert!(!failed.error_message.is_empty());
    assert!(
        store
            .list_operation_logs("op-apply-failure-store")
            .expect("operation logs")
            .iter()
            .any(
                |record| record.level == "error" && record.message.contains("service.start failed")
            )
    );
}

#[test]
fn operation_rollback_updates_store() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway.clone()).expect("put gateway");
    let operation = confirm_operation(
        &release_install_operation("op-rollback-store", &gateway, &[])
            .expect("release install operation"),
    )
    .expect("confirm operation");
    store.put_operation(operation).expect("put operation");
    OperationExecutor::new(&mut store)
        .apply("op-rollback-store")
        .expect("apply operation");

    let rolled_back = OperationExecutor::new(&mut store)
        .rollback("op-rollback-store")
        .expect("rollback operation");
    assert_eq!(rolled_back.status, OperationStatus::RolledBack);
    assert_eq!(rolled_back.rolled_back_at, "rolled_back");
    assert_eq!(rolled_back.updated_at, "rolled_back");
    assert!(
        store
            .list_operation_logs("op-rollback-store")
            .expect("operation logs")
            .iter()
            .any(|record| record.step_id.starts_with("rollback:"))
    );
}

#[test]
fn operation_logs_can_be_reopened() {
    let mut store = MemoryOrchestratorStore::new();
    let operation = plan_operation(
        "op-log-source",
        "operation.create",
        "Operation",
        "gateway",
        serde_json::json!({}),
        serde_json::json!({"steps": [{"action": "plan", "target": "gateway"}]}),
        serde_json::json!({"steps": []}),
    )
    .expect("plan operation");
    store.put_operation(operation).expect("put operation");
    store
        .append_operation_log(operation_step_log_record(
            "op-log-source",
            "step-1",
            "info",
            "first log",
            serde_json::json!({"seq": 1}),
        ))
        .expect("append log");

    let reopened = store
        .list_operation_logs("op-log-source")
        .expect("reopen operation logs");
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].step_id, "step-1");
    assert_eq!(reopened[0].created_at, "log-1");
}

#[test]
fn workbench_uses_store_backed_operation_lifecycle() {
    let root = repo_root();
    let context = crate::workbench::load_operation_workbench_context_with_database_url(&root, None)
        .expect("workbench context")
        .with_memory_store();
    let session = context
        .build_session("release.install")
        .expect("release install session");
    assert_eq!(session.current_operation.status, OperationStatus::Planned);
    assert_eq!(session.current_operation.created_at, "planned");

    let confirmed = context.confirm(&session).expect("confirm through context");
    assert_eq!(
        confirmed.current_operation.status,
        OperationStatus::AwaitingConfirmation
    );
    let applied = context.apply(&confirmed).expect("apply through context");
    assert_eq!(applied.current_operation.status, OperationStatus::Succeeded);
    assert!(!applied.logs.is_empty());
    let rolled_back = context
        .rollback(&applied)
        .expect("rollback through context");
    assert_eq!(
        rolled_back.current_operation.status,
        OperationStatus::RolledBack
    );
    assert!(rolled_back.logs.len() > applied.logs.len());
}

#[test]
fn operation_lock_prevents_parallel_apply() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway.clone()).expect("put gateway");
    let operation = confirm_operation(
        &release_install_operation("op-parallel-lock", &gateway, &[])
            .expect("release install operation"),
    )
    .expect("confirm operation");
    store.put_operation(operation).expect("put operation");
    store
        .acquire_operation_lock(OperationLock {
            lock_key: "operation:op-parallel-lock".to_string(),
            operation_id: "op-parallel-lock".to_string(),
            owner: "test".to_string(),
            expires_at: "session".to_string(),
            created_at: String::new(),
        })
        .expect("acquire lock");

    let blocked = OperationExecutor::new(&mut store)
        .apply("op-parallel-lock")
        .expect_err("locked operation should not apply");
    assert!(blocked.to_string().contains("is locked"));
}

#[test]
fn operation_executor_logs_rollback_mutation_failure() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway service");
    let mut problem_api = valid_service();
    problem_api.id = "problem-service".to_string();
    store
        .put_service(problem_api)
        .expect("put problem-service service");
    let source = Endpoint {
        endpoint: "127.0.0.1:18080:gateway".to_string(),
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
    };
    let target = Endpoint {
        endpoint: "127.0.0.1:18081:problem-service".to_string(),
        service_id: "problem-service".to_string(),
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
    store
        .put_endpoint(source.clone())
        .expect("put source endpoint");
    store
        .put_endpoint(target.clone())
        .expect("put target endpoint");
    let link = Link {
        source_endpoint: source.endpoint,
        target_endpoint: target.endpoint,
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
    let operation = confirm_operation(
        &link_create_operation("op-rollback-fails", &link, &store.endpoints())
            .expect("link create operation"),
    )
    .expect("confirm link operation");
    store.put_operation(operation).expect("put operation");
    let applied = OperationExecutor::new(&mut store)
        .apply("op-rollback-fails")
        .expect("apply link operation");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    store
        .delete_link(&link.source_endpoint, &link.target_endpoint)
        .expect("remove link before rollback");

    let failed = OperationExecutor::new(&mut store)
        .rollback("op-rollback-fails")
        .expect_err("rollback mutation should fail");
    assert!(failed.to_string().contains("not found"));
    assert!(
        store
            .list_operation_logs("op-rollback-fails")
            .expect("operation logs")
            .iter()
            .any(|record| record.level == "error"
                && record
                    .message
                    .contains("operation link.create rollback failed"))
    );
    assert_eq!(
        store
            .get_operation("op-rollback-fails")
            .expect("get operation")
            .expect("operation")
            .status,
        OperationStatus::Succeeded,
        "failed rollback must not mark the original operation rolled back"
    );
    assert!(
        store
            .acquire_operation_lock(OperationLock {
                lock_key: "operation:op-rollback-fails".to_string(),
                operation_id: "op-rollback-fails".to_string(),
                owner: "test".to_string(),
                expires_at: "session".to_string(),
                created_at: String::new(),
            })
            .expect("lock can be acquired after rollback failure"),
        "rollback failure must release operation lock"
    );
}

#[test]
fn operation_executor_rejects_rollback_when_operation_is_locked() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway service");
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:19110:gateway".to_string(),
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
    let operation = endpoint_create_operation("op-rollback-locked", &endpoint)
        .expect("endpoint create operation");
    store.put_operation(operation).expect("put operation");
    OperationExecutor::new(&mut store)
        .apply("op-rollback-locked")
        .expect("apply operation");
    assert!(
        store
            .acquire_operation_lock(OperationLock {
                lock_key: "operation:op-rollback-locked".to_string(),
                operation_id: "op-rollback-locked".to_string(),
                owner: "test".to_string(),
                expires_at: "session".to_string(),
                created_at: String::new(),
            })
            .expect("manual lock")
    );

    let blocked = OperationExecutor::new(&mut store)
        .rollback("op-rollback-locked")
        .expect_err("rollback should honor operation lock");
    assert!(blocked.to_string().contains("is locked"));
    assert_eq!(
        store
            .get_operation("op-rollback-locked")
            .expect("get operation")
            .expect("operation")
            .status,
        OperationStatus::Succeeded
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
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            operation_id: String::new(),
            path: "/logs".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Gateway logs".to_string(),
        })
        .expect("put scoped log view");
    store
        .put_operation(
            log_create_operation("op-log-query", "gateway", Some("127.0.0.1:8080:gateway"))
                .expect("service log operation"),
        )
        .expect("put operation");
    let mut middle_log = operation_step_log_record(
        "op-log-query",
        "step-middle",
        "info",
        "middle service log collected",
        serde_json::json!({"service_id": "gateway"}),
    );
    middle_log.created_at = "2026-01-01T12:00:00Z".to_string();
    store
        .append_operation_log(middle_log)
        .expect("append operation log");
    let mut newer_log = operation_step_log_record(
        "op-log-query",
        "step-new",
        "info",
        "newer service log collected",
        serde_json::json!({"service_id": "gateway"}),
    );
    newer_log.created_at = "2026-01-02T00:00:00Z".to_string();
    store
        .append_operation_log(newer_log)
        .expect("append newer operation log");
    let mut older_log = operation_step_log_record(
        "op-log-query",
        "step-older",
        "info",
        "oldest service log collected",
        serde_json::json!({"service_id": "gateway"}),
    );
    older_log.created_at = "2026-01-01T00:00:00Z".to_string();
    store
        .append_operation_log(older_log)
        .expect("append older operation log");

    let result = query_logs(
        &store,
        &LogQuery {
            service_id: Some("gateway".to_string()),
            endpoint: Some("127.0.0.1:8080:gateway".to_string()),
            operation_id: Some("op-log-query".to_string()),
            source_id: None,
        },
    )
    .expect("query logs");
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.operation_logs.len(), 3);
    assert_eq!(result.operation_logs[0].step_id, "step-new");
    assert_eq!(result.operation_logs[1].step_id, "step-middle");
    assert_eq!(result.operation_logs[2].step_id, "step-older");

    assert!(
        store
            .put_log_view(LogView {
                source_id: "bad".to_string(),
                service_id: "gateway".to_string(),
                endpoint: "127.0.0.1:8080:gateway".to_string(),
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
        "LogView endpoint identity must remain ip:port:service-name"
    );
    assert!(
        validate_log_view(&LogView {
            source_id: "wrong-service-endpoint".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "127.0.0.1:8081:problem-service".to_string(),
            operation_id: String::new(),
            path: "/logs".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: String::new(),
        })
        .is_err(),
        "LogView service_id must match endpoint third segment"
    );
}

#[test]
fn operation_executor_materializes_operation_log_view_and_diagnostic_export() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
    let source_operation =
        log_create_operation("op-source-logs", "gateway", Some("127.0.0.1:8080:gateway"))
            .expect("source log operation");
    store
        .put_operation(source_operation.clone())
        .expect("put source operation");
    store
        .append_operation_log(operation_step_log_record(
            "op-source-logs",
            "collect",
            "info",
            "source operation log",
            serde_json::json!({"service_id": "gateway"}),
        ))
        .expect("append source log");

    let view_operation = log_query_operation("op-open-source-logs", "op-source-logs")
        .expect("operation logs view operation");
    store
        .put_operation(view_operation)
        .expect("put log view operation");
    let applied = OperationExecutor::new(&mut store)
        .apply("op-open-source-logs")
        .expect("apply operation log view");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    let log_view = store
        .log_views()
        .into_iter()
        .find(|view| view.source_id == "operation:op-source-logs")
        .expect("operation-scoped log view");
    assert_eq!(log_view.endpoint, "127.0.0.1:8080:gateway");
    assert_eq!(log_view.read_policy, "operation-scoped");
    assert!(
        store
            .operation_logs("op-open-source-logs")
            .iter()
            .any(|record| record.step_id == "log.query"
                && record
                    .data
                    .get("log_count")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)),
        "log.query should record the number of visible operation logs"
    );

    let report =
        build_diagnostic_report(&store, "diag-observable").expect("build diagnostic report");
    store
        .put_diagnostic_report(report)
        .expect("put diagnostic report");
    let export_operation = diagnostic_export_operation("op-export-diag", "diag-observable", "json")
        .expect("diagnostics export operation");
    store
        .put_operation(export_operation)
        .expect("put export operation");
    let exported = OperationExecutor::new(&mut store)
        .apply("op-export-diag")
        .expect("apply diagnostic export");
    assert_eq!(exported.status, OperationStatus::Succeeded);
    assert!(
        store
            .operation_logs("op-export-diag")
            .iter()
            .any(|record| record.step_id == "diagnostic.export"
                && record
                    .data
                    .get("format")
                    .and_then(serde_json::Value::as_str)
                    == Some("json")
                && record
                    .data
                    .get("content_bytes")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|value| value > 0)),
        "diagnostic.export should record export metadata without storing arbitrary files"
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

    let err = OperationExecutor::new(&mut store)
        .apply("op-start-1")
        .expect_err(
            "service.start must not report success when fixed driver execution is disabled",
        );
    assert!(err.to_string().contains("execution is not enabled"));
    assert_eq!(
        store.operation("op-start-1").map(|item| &item.status),
        Some(&OperationStatus::Failed)
    );
}

#[test]
fn service_start_uses_driver() {
    let _guard = DOCKER_BINARY_ENV_LOCK.lock().expect("docker env lock");
    let previous = std::env::var("OJOS_ORCHESTRATOR_DOCKER_BINARY").ok();
    unsafe {
        std::env::set_var(
            "OJOS_ORCHESTRATOR_DOCKER_BINARY",
            "ojos-docker-compose-missing",
        );
    }

    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "gateway".to_string();
    service.runtime.mode = RuntimeMode::Container;
    service.runtime.driver = "compose".to_string();
    store.put_service(service).expect("put service");
    let operation =
        service_lifecycle_operation("op-service-start-driver", "service.start", "gateway")
            .expect("start operation");
    store.put_operation(operation).expect("put operation");

    let err = OperationExecutor::new(&mut store)
        .with_service_driver_execution_enabled()
        .apply("op-service-start-driver")
        .expect_err("missing docker binary should fail fixed driver execution");
    assert!(
        err.to_string()
            .contains("docker compose fixed command failed to start")
    );
    assert!(
        store
            .operation("op-service-start-driver")
            .expect("stored operation")
            .error_message
            .contains("fixed command failed to start")
    );

    unsafe {
        match previous {
            Some(value) => std::env::set_var("OJOS_ORCHESTRATOR_DOCKER_BINARY", value),
            None => std::env::remove_var("OJOS_ORCHESTRATOR_DOCKER_BINARY"),
        }
    }
}

#[test]
fn service_stop_uses_driver() {
    let compose = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
    let command = compose
        .command_for("service.stop", "gateway")
        .expect("service.stop fixed command");
    assert!(command.contains(&"stop".to_string()));
    assert_eq!(command.last().map(String::as_str), Some("gateway"));
}

#[test]
fn service_restart_uses_driver() {
    let compose = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
    let command = compose
        .command_for("service.restart", "gateway")
        .expect("service.restart fixed command");
    assert!(command.contains(&"restart".to_string()));
    assert_eq!(command.last().map(String::as_str), Some("gateway"));
}

#[test]
fn service_logs_view_uses_log_source() {
    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "gateway".to_string();
    store.put_service(service).expect("put service");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
        })
        .expect("put endpoint");
    let operation = log_create_operation(
        "op-service-logs-driver",
        "gateway",
        Some("127.0.0.1:8080:gateway"),
    )
    .expect("logs view operation");
    store.put_operation(operation).expect("put operation");

    let applied = OperationExecutor::new(&mut store)
        .apply("op-service-logs-driver")
        .expect("service logs view should materialize LogView");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    assert!(
        store
            .log_views()
            .iter()
            .any(|view| view.source_id == "gateway:127.0.0.1:8080:gateway"
                && view.endpoint == "127.0.0.1:8080:gateway"),
        "log.create should persist a scoped LogView"
    );
}

#[test]
fn service_lifecycle_failure_is_recorded() {
    let _guard = DOCKER_BINARY_ENV_LOCK.lock().expect("docker env lock");
    let previous = std::env::var("OJOS_ORCHESTRATOR_DOCKER_BINARY").ok();
    unsafe {
        std::env::set_var(
            "OJOS_ORCHESTRATOR_DOCKER_BINARY",
            "ojos-docker-compose-missing",
        );
    }

    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "gateway".to_string();
    service.runtime.mode = RuntimeMode::Container;
    service.runtime.driver = "compose".to_string();
    store.put_service(service).expect("put service");
    let operation =
        service_lifecycle_operation("op-service-driver-failure", "service.start", "gateway")
            .expect("start operation");
    store.put_operation(operation).expect("put operation");

    let failed = OperationExecutor::new(&mut store)
        .with_service_driver_execution_enabled()
        .apply("op-service-driver-failure")
        .expect_err("missing docker binary should fail");
    assert!(failed.to_string().contains("fixed command failed to start"));
    assert_eq!(
        store
            .operation("op-service-driver-failure")
            .map(|operation| &operation.status),
        Some(&OperationStatus::Failed)
    );
    assert!(
        store
            .operation_logs("op-service-driver-failure")
            .iter()
            .any(|record| record.level == "error"
                && record.message.contains("operation service.start failed")),
        "driver failure must be recorded in operation logs"
    );

    unsafe {
        match previous {
            Some(value) => std::env::set_var("OJOS_ORCHESTRATOR_DOCKER_BINARY", value),
            None => std::env::remove_var("OJOS_ORCHESTRATOR_DOCKER_BINARY"),
        }
    }
}

#[test]
fn service_lifecycle_unsupported_is_not_success() {
    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "postgresql".to_string();
    service.runtime.mode = RuntimeMode::External;
    service.runtime.driver = "external".to_string();
    store.put_service(service).expect("put service");
    let operation = service_lifecycle_operation("op-external-start", "service.start", "postgresql")
        .expect("start operation");
    store.put_operation(operation).expect("put operation");

    let err = OperationExecutor::new(&mut store)
        .apply("op-external-start")
        .expect_err("external endpoint service start is unsupported");
    assert!(err.to_string().contains("cannot control service lifecycle"));
    assert_eq!(
        store
            .operation("op-external-start")
            .map(|operation| &operation.status),
        Some(&OperationStatus::Failed)
    );
}

#[test]
fn operation_executor_releases_lock_after_apply_failure() {
    let mut store = MemoryOrchestratorStore::new();
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:19090:gateway".to_string(),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Missing Service Endpoint".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let operation =
        endpoint_create_operation("op-apply-fails", &endpoint).expect("endpoint create operation");
    store.put_operation(operation).expect("put operation");

    let failed = OperationExecutor::new(&mut store)
        .apply("op-apply-fails")
        .expect_err("missing service should fail inside apply mutation");
    assert!(failed.to_string().contains("gateway"));
    let stored = store.operation("op-apply-fails").expect("stored operation");
    assert_eq!(stored.status, OperationStatus::Failed);
    assert!(stored.error_message.contains("gateway"));
    assert!(store.operation_logs("op-apply-fails").iter().any(|record| {
        record.level == "error" && record.message.contains("operation endpoint.create failed")
    }));
    assert!(
        store
            .acquire_operation_lock(OperationLock {
                lock_key: "operation:op-apply-fails".to_string(),
                operation_id: "op-apply-fails".to_string(),
                owner: "test".to_string(),
                expires_at: "session".to_string(),
                created_at: String::new(),
            })
            .expect("lock can be acquired after failure"),
        "apply failure must release operation lock"
    );
}

#[test]
fn operation_executor_mutates_core_store_objects() {
    let root = repo_root();
    let mut store = MemoryOrchestratorStore::new();
    let gateway =
        validate_service_manifest_file(&root, Path::new("services/gateway/service.yaml")).unwrap();
    let problem_api =
        validate_service_manifest_file(&root, Path::new("services/problem-service/service.yaml"))
            .unwrap();

    let install = release_install_operation("op-install-gateway", &gateway, &[])
        .expect("release install operation");
    let install = confirm_operation(&install).expect("confirm install");
    store.put_operation(install).expect("put install");
    OperationExecutor::new(&mut store)
        .apply("op-install-gateway")
        .expect("apply install");
    assert!(store.service("gateway").is_some());

    store
        .put_service(problem_api.clone())
        .expect("put problem-service service");
    let gateway_endpoint = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
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
    let endpoint_op = endpoint_create_operation("op-endpoint", &gateway_endpoint)
        .expect("endpoint create operation");
    store.put_operation(endpoint_op).expect("put endpoint op");
    OperationExecutor::new(&mut store)
        .apply("op-endpoint")
        .expect("apply endpoint");
    assert!(store.endpoint("127.0.0.1:8080:gateway").is_some());

    let problem_endpoint = Endpoint {
        endpoint: "127.0.0.1:8081:problem-service".to_string(),
        service_id: "problem-service".to_string(),
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
        source_endpoint: "127.0.0.1:8080:gateway".to_string(),
        target_endpoint: "127.0.0.1:8081:problem-service".to_string(),
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
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
        store.endpoints(),
        store.links(),
        Vec::new(),
        vec![LogView {
            source_id: "gateway:health".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            operation_id: String::new(),
            path: "/health".to_string(),
            driver: "external-endpoint".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Gateway health".to_string(),
        }],
        vec![DiagnosticReport {
            report_id: "diag-topology".to_string(),
            target_type: "Topology".to_string(),
            target_id: "127.0.0.1:8080:gateway".to_string(),
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
    assert!(store.topology("127.0.0.1:8080:gateway").is_some());
    assert_eq!(store.log_views().len(), 1);
    assert_eq!(store.diagnostic_reports().len(), 1);
}

fn dispatcher_store_with_services() -> MemoryOrchestratorStore {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    gateway.name = "Gateway".to_string();
    let mut auth = valid_service();
    auth.id = "auth-service".to_string();
    auth.name = "Auth Service".to_string();
    let mut problem_api = valid_service();
    problem_api.id = "problem-service".to_string();
    problem_api.name = "Problem API".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(auth).expect("put auth");
    store.put_service(problem_api).expect("put problem api");
    store
}

fn request(action: &str, operation_id: &str, fields: &[(&str, &str)]) -> ActionRequest {
    ActionRequest::new(
        operation_id,
        action,
        fields
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    )
}

#[test]
fn action_dispatcher_routes_schema_actions() {
    let root = repo_root();
    let schemas = load_shared_schemas(&root).expect("schemas");
    let catalog = validate_action_catalog(&schemas).expect("catalog");
    let matrix = action_matrix();
    assert_eq!(matrix.len(), catalog.len());
    for action in schemas.actions {
        assert!(
            matrix.iter().any(|entry| entry.action_id == action
                && entry.gui_entry
                && entry.tui_entry
                && !entry.action_id.contains("machine")),
            "missing matrix entry for {action}"
        );
    }
}

#[test]
fn action_result_marks_unsupported_without_success() {
    let mut store = dispatcher_store_with_services();
    let result = OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "service.start",
            "op-unsupported-start",
            &[("service_id", "gateway")],
        ))
        .expect("unsupported result");
    assert_eq!(
        result.capability_status,
        ActionCapabilityStatus::Unsupported
    );
    assert_eq!(result.status, "UNSUPPORTED");
    assert!(!result.message.contains("成功"));
    let operation = store
        .operation("op-unsupported-start")
        .expect("stored unsupported operation");
    assert_eq!(operation.status, OperationStatus::Failed);
    assert!(
        store
            .operation_logs("op-unsupported-start")
            .iter()
            .any(|record| {
                record.level == "warn"
                    && record.step_id == "unsupported"
                    && record
                        .data
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        == Some("UNSUPPORTED")
            })
    );
}

#[test]
fn unsupported_catalog_actions_never_enter_fake_success_path() {
    let mut store = dispatcher_store_with_services();
    let result = OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "topology.create",
            "op-unsupported-deployment",
            &[
                ("name", "default-topology"),
                ("root_endpoint", "127.0.0.1:8080:gateway"),
                ("confirm", "true"),
            ],
        ))
        .expect("unsupported deployment result");
    assert_eq!(
        result.capability_status,
        ActionCapabilityStatus::Unsupported
    );
    assert_eq!(result.status, "UNSUPPORTED");
    assert!(result.message.contains("已阻止假成功路径"));
    let operation = store
        .operation("op-unsupported-deployment")
        .expect("stored unsupported operation");
    assert_eq!(operation.status, OperationStatus::Failed);
    assert_eq!(
        operation
            .result
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("UNSUPPORTED")
    );
    assert!(
        store
            .operation_logs("op-unsupported-deployment")
            .iter()
            .any(|record| record.level == "warn" && record.message.contains("已阻止假成功路径")),
        "unsupported catalog action should be recorded as a warning log"
    );
}

#[test]
fn endpoint_create_update_delete_and_health_write_store() {
    let mut store = dispatcher_store_with_services();
    let mut dispatcher =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe);
    let registered = dispatcher
        .dispatch(request(
            "endpoint.create",
            "op-endpoint-register-console",
            &[
                ("endpoint", "127.0.0.1:8080:gateway"),
                ("service_id", "gateway"),
                ("protocol", "http"),
                ("health_path", "/health"),
                ("display_name", "Local Gateway"),
                ("note", "本机 Gateway"),
                ("config", r#"{"region":"local"}"#),
            ],
        ))
        .expect("endpoint create");
    assert_eq!(
        registered.capability_status,
        ActionCapabilityStatus::StoreBacked
    );
    assert!(
        registered
            .changed_objects
            .contains(&"Endpoint:127.0.0.1:8080:gateway".to_string())
    );
    assert!(store.endpoint("127.0.0.1:8080:gateway").is_some());
    assert_eq!(
        store
            .endpoint("127.0.0.1:8080:gateway")
            .expect("registered endpoint")
            .config
            .get("region")
            .and_then(serde_json::Value::as_str),
        Some("local")
    );

    let updated =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "endpoint.update",
                "op-endpoint-update-console",
                &[
                    ("endpoint", "127.0.0.1:8080:gateway"),
                    ("protocol", "tcp"),
                    ("health_path", "/ready"),
                    ("display_name", "Gateway TCP"),
                    ("note", "更新后的 Endpoint"),
                    ("config", r#"{"region":"updated"}"#),
                    ("confirm", "true"),
                ],
            ))
            .expect("endpoint update");
    assert_eq!(
        updated.capability_status,
        ActionCapabilityStatus::StoreBacked
    );
    assert_eq!(
        store
            .endpoint("127.0.0.1:8080:gateway")
            .expect("updated endpoint")
            .protocol,
        "tcp"
    );
    assert_eq!(
        store
            .endpoint("127.0.0.1:8080:gateway")
            .expect("updated endpoint")
            .config
            .get("region")
            .and_then(serde_json::Value::as_str),
        Some("updated")
    );

    let health = OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "endpoint.health.check",
            "op-endpoint-health-console",
            &[("endpoint", "127.0.0.1:8080:gateway")],
        ))
        .expect("endpoint health");
    assert_eq!(health.capability_status, ActionCapabilityStatus::Real);
    assert_eq!(
        store
            .endpoint("127.0.0.1:8080:gateway")
            .expect("health endpoint")
            .health,
        "unreachable"
    );

    let deleted =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "endpoint.delete",
                "op-endpoint-delete-console",
                &[("endpoint", "127.0.0.1:8080:gateway"), ("confirm", "true")],
            ))
            .expect("endpoint delete");
    assert_eq!(
        deleted.capability_status,
        ActionCapabilityStatus::StoreBacked
    );
    assert!(store.endpoint("127.0.0.1:8080:gateway").is_none());
}

#[test]
fn link_create_update_delete_and_health_write_store() {
    let mut store = dispatcher_store_with_services();
    for (endpoint, service_id, reachable) in [
        ("127.0.0.1:8080:gateway", "gateway", true),
        ("127.0.0.1:8001:auth-service", "auth-service", true),
    ] {
        store
            .put_endpoint(Endpoint {
                endpoint: endpoint.to_string(),
                service_id: service_id.to_string(),
                protocol: "http".to_string(),
                health_path: "/health".to_string(),
                health: if reachable { "healthy" } else { "unknown" }.to_string(),
                reachable,
                display_name: service_id.to_string(),
                note: String::new(),
                config: serde_json::json!({}),
                created_at: String::new(),
                updated_at: String::new(),
            })
            .expect("put endpoint");
    }

    let created =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "link.create",
                "op-link-create-console",
                &[
                    ("source_endpoint", "127.0.0.1:8080:gateway"),
                    ("target_endpoint", "127.0.0.1:8001:auth-service"),
                    ("protocol", "http"),
                    ("auth_mode", "internal"),
                    ("scope", "gateway-to-auth-service"),
                    ("config_ref", "config://gateway/auth-service"),
                    ("secret_ref", "secret://gateway/auth-service"),
                    ("policy", r#"{"required":true}"#),
                    ("confirm", "true"),
                ],
            ))
            .expect("link create");
    assert_eq!(
        created.capability_status,
        ActionCapabilityStatus::StoreBacked
    );
    assert!(
        store
            .get_link("127.0.0.1:8080:gateway", "127.0.0.1:8001:auth-service")
            .expect("get link")
            .is_some()
    );
    let stored_link = store
        .get_link("127.0.0.1:8080:gateway", "127.0.0.1:8001:auth-service")
        .expect("get link")
        .expect("link");
    assert_eq!(stored_link.config_ref, "config://gateway/auth-service");
    assert_eq!(stored_link.secret_ref, "secret://gateway/auth-service");
    assert_eq!(
        stored_link
            .policy
            .get("required")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "link.update",
            "op-link-update-console",
            &[
                ("source_endpoint", "127.0.0.1:8080:gateway"),
                ("target_endpoint", "127.0.0.1:8001:auth-service"),
                ("protocol", "http"),
                ("auth_mode", "none"),
                ("scope", ""),
                ("confirm", "true"),
            ],
        ))
        .expect("link update");
    assert_eq!(
        store
            .get_link("127.0.0.1:8080:gateway", "127.0.0.1:8001:auth-service")
            .expect("get link")
            .expect("link")
            .auth_mode,
        "none"
    );

    let health = OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "link.health.check",
            "op-link-health-console",
            &[
                ("source_endpoint", "127.0.0.1:8080:gateway"),
                ("target_endpoint", "127.0.0.1:8001:auth-service"),
            ],
        ))
        .expect("link health");
    assert_eq!(health.capability_status, ActionCapabilityStatus::Real);
    assert_eq!(
        store
            .get_link("127.0.0.1:8080:gateway", "127.0.0.1:8001:auth-service")
            .expect("get link")
            .expect("link")
            .health,
        "degraded",
        "empty scope/auth policy should not be reported as fake healthy"
    );

    OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "link.delete",
            "op-link-delete-console",
            &[
                ("source_endpoint", "127.0.0.1:8080:gateway"),
                ("target_endpoint", "127.0.0.1:8001:auth-service"),
                ("confirm", "true"),
            ],
        ))
        .expect("link delete");
    assert!(
        store
            .get_link("127.0.0.1:8080:gateway", "127.0.0.1:8001:auth-service")
            .expect("get link")
            .is_none()
    );
}

#[derive(Clone, Copy)]
enum RegistryKind {
    Route,
    Frontend,
    Migration,
    Permission,
    Redis,
    Storage,
    Config,
}

impl RegistryKind {
    fn label(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Frontend => "frontend",
            Self::Migration => "migration",
            Self::Permission => "permission",
            Self::Redis => "redis",
            Self::Storage => "storage",
            Self::Config => "config",
        }
    }

    fn create_fields(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Route => vec![
                ("route_id", "gateway-auth"),
                ("service_id", "gateway"),
                ("path", "/api/auth/**"),
                ("method", "ANY"),
                ("target", "gateway[*]"),
                ("permission", "gateway.read"),
            ],
            Self::Frontend => vec![
                ("frontend_id", "gateway-shell"),
                ("service_id", "gateway"),
                ("route_prefix", "/"),
                ("remote_entry", "/assets/gateway/remoteEntry.js"),
            ],
            Self::Migration => vec![
                ("migration_id", "gateway-0001"),
                ("service_id", "gateway"),
                ("version", "0001"),
                ("checksum", "sha256:old"),
            ],
            Self::Permission => vec![
                ("permission_id", "gateway.read"),
                ("service_id", "gateway"),
                ("source", "manual"),
            ],
            Self::Redis => vec![
                ("resource_id", "gateway-events"),
                ("service_id", "gateway"),
                ("kind", "stream"),
                ("usage", "events"),
            ],
            Self::Storage => vec![
                ("resource_id", "gateway-object"),
                ("service_id", "gateway"),
                ("bucket", "gateway-bucket"),
                ("path_prefix", "/gateway"),
            ],
            Self::Config => vec![
                ("config_id", "gateway-default"),
                ("service_id", "gateway"),
                ("version", "default"),
                ("config", r#"{"mode":"old"}"#),
            ],
        }
    }

    fn update_fields(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Route => vec![
                ("route_id", "gateway-auth"),
                ("service_id", "gateway"),
                ("path", "/api/auth/**"),
                ("method", "ANY"),
                ("target", "gateway[*]"),
                ("permission", "gateway.admin"),
            ],
            Self::Frontend => vec![
                ("frontend_id", "gateway-shell"),
                ("service_id", "gateway"),
                ("route_prefix", "/"),
                ("remote_entry", "/assets/gateway/v2/remoteEntry.js"),
            ],
            Self::Migration => vec![
                ("migration_id", "gateway-0001"),
                ("service_id", "gateway"),
                ("version", "0001"),
                ("checksum", "sha256:new"),
            ],
            Self::Permission => vec![
                ("permission_id", "gateway.read"),
                ("service_id", "gateway"),
                ("source", "release"),
            ],
            Self::Redis => vec![
                ("resource_id", "gateway-events"),
                ("service_id", "gateway"),
                ("kind", "consumer-group"),
                ("usage", "updated events"),
            ],
            Self::Storage => vec![
                ("resource_id", "gateway-object"),
                ("service_id", "gateway"),
                ("bucket", "gateway-bucket-v2"),
                ("path_prefix", "/gateway/v2"),
            ],
            Self::Config => vec![
                ("config_id", "gateway-default"),
                ("service_id", "gateway"),
                ("version", "default"),
                ("config", r#"{"mode":"new"}"#),
            ],
        }
    }

    fn delete_fields(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Route => vec![("route_id", "ANY /api/auth/**")],
            Self::Frontend => vec![("frontend_id", "gateway:/")],
            Self::Migration => vec![("migration_id", "gateway@0001")],
            Self::Permission => vec![("permission_id", "gateway.read")],
            Self::Redis => vec![("resource_id", "gateway:gateway-events")],
            Self::Storage => vec![("resource_id", "gateway:gateway-object:gateway-bucket")],
            Self::Config => vec![("config_id", "gateway@default")],
        }
    }

    fn exists(self, store: &MemoryOrchestratorStore) -> bool {
        match self {
            Self::Route => store
                .service_routes()
                .iter()
                .any(|item| item.path == "/api/auth/**" && item.target_service_name == "gateway"),
            Self::Frontend => store
                .service_frontend_entries()
                .iter()
                .any(|item| item.service_name == "gateway"),
            Self::Migration => store
                .service_migration_records()
                .iter()
                .any(|item| item.service_name == "gateway" && item.migration_version == "0001"),
            Self::Permission => store.service_permission_records().iter().any(|item| {
                item.service_name == "gateway" && item.permission_key == "gateway.read"
            }),
            Self::Redis => store
                .service_redis_resources()
                .iter()
                .any(|item| item.service_name == "gateway" && item.name == "gateway-events"),
            Self::Storage => store
                .service_storage_resources()
                .iter()
                .any(|item| item.service_name == "gateway" && item.object_type == "gateway-object"),
            Self::Config => store
                .rendered_service_configs()
                .iter()
                .any(|item| item.service_name == "gateway" && item.version == "default"),
        }
    }

    fn updated(self, store: &MemoryOrchestratorStore) -> bool {
        match self {
            Self::Route => store
                .service_routes()
                .iter()
                .any(|item| item.path == "/api/auth/**" && item.permission == "gateway.admin"),
            Self::Frontend => store
                .service_frontend_entries()
                .iter()
                .any(|item| item.remote_entry == "/assets/gateway/v2/remoteEntry.js"),
            Self::Migration => store
                .service_migration_records()
                .iter()
                .any(|item| item.checksum == "sha256:new"),
            Self::Permission => store
                .service_permission_records()
                .iter()
                .any(|item| item.permission_key == "gateway.read" && item.source == "release"),
            Self::Redis => store
                .service_redis_resources()
                .iter()
                .any(|item| item.name == "gateway-events" && item.kind == "consumer-group"),
            Self::Storage => store.service_storage_resources().iter().any(|item| {
                item.object_type == "gateway-object" && item.bucket == "gateway-bucket-v2"
            }),
            Self::Config => store.rendered_service_configs().iter().any(|item| {
                item.config.get("mode").and_then(serde_json::Value::as_str) == Some("new")
            }),
        }
    }
}

fn dispatch_confirmed(
    store: &mut MemoryOrchestratorStore,
    action: &str,
    operation_id: &str,
    fields: Vec<(&str, &str)>,
) {
    let mut fields = fields;
    fields.push(("confirm", "true"));
    OrchestratorActionDispatcher::with_endpoint_probe(store, StaticEndpointProbe)
        .dispatch(request(action, operation_id, &fields))
        .expect("dispatch confirmed registry action");
}

#[test]
fn registry_subresource_crud_apply_and_rollback_paths_are_store_backed() {
    for kind in [
        RegistryKind::Route,
        RegistryKind::Frontend,
        RegistryKind::Migration,
        RegistryKind::Permission,
        RegistryKind::Redis,
        RegistryKind::Storage,
        RegistryKind::Config,
    ] {
        let mut store = dispatcher_store_with_services();
        let label = kind.label();

        dispatch_confirmed(
            &mut store,
            &format!("{label}.create"),
            &format!("op-{label}-create"),
            kind.create_fields(),
        );
        assert!(
            kind.exists(&store),
            "{label}.create should persist registry resource"
        );
        OperationExecutor::new(&mut store)
            .rollback(&format!("op-{label}-create"))
            .expect("rollback create");
        assert!(
            !kind.exists(&store),
            "{label}.create rollback should remove newly-created registry resource"
        );

        dispatch_confirmed(
            &mut store,
            &format!("{label}.create"),
            &format!("op-{label}-create-again"),
            kind.create_fields(),
        );
        dispatch_confirmed(
            &mut store,
            &format!("{label}.update"),
            &format!("op-{label}-update"),
            kind.update_fields(),
        );
        assert!(
            kind.updated(&store),
            "{label}.update should mutate registry resource"
        );
        OperationExecutor::new(&mut store)
            .rollback(&format!("op-{label}-update"))
            .expect("rollback update");
        assert!(
            kind.exists(&store) && !kind.updated(&store),
            "{label}.update rollback should restore previous registry state"
        );

        dispatch_confirmed(
            &mut store,
            &format!("{label}.delete"),
            &format!("op-{label}-delete"),
            kind.delete_fields(),
        );
        assert!(
            !kind.exists(&store),
            "{label}.delete should remove registry resource"
        );
        OperationExecutor::new(&mut store)
            .rollback(&format!("op-{label}-delete"))
            .expect("rollback delete");
        assert!(
            kind.exists(&store),
            "{label}.delete rollback should restore registry resource"
        );
    }
}

#[test]
fn set_expand_apply_are_not_formal_console_actions() {
    let root = repo_root();
    let mut store = dispatcher_store_with_services();
    let set = validate_deployment_template_file(&root, Path::new("sets/single-node-oj.yaml"))
        .expect("set");
    for service in &set.services {
        let path = Path::new("services")
            .join(service.id())
            .join("service.yaml");
        let manifest = validate_service_manifest_file(&root, &path).expect("service manifest");
        store.put_service(manifest).expect("put set service");
    }
    let expanded =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "set.expand",
                "op-set-expand-console",
                &[("set_id", "single-node-oj")],
            ))
            .expect_err("set expand is not a formal action");
    assert!(expanded.to_string().contains("unknown action set.expand"));

    let applied =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "set.apply",
                "op-set-apply-console",
                &[("set_id", "single-node-oj"), ("confirm", "true")],
            ))
            .expect_err("set apply is not a formal action");
    assert!(
        applied.to_string().contains("unknown action set.apply"),
        "service-name endpoint groups are derived queries, not formal actions"
    );
}

#[test]
fn operation_plan_confirm_apply_rollback_and_logs_are_visible() {
    let mut store = dispatcher_store_with_services();
    let planned =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "endpoint.create",
                "op-operation-lifecycle-console",
                &[
                    ("endpoint", "127.0.0.1:18080:gateway"),
                    ("service_id", "gateway"),
                    ("protocol", "http"),
                    ("health_path", "/health"),
                ],
            ))
            .expect("endpoint create");
    assert_eq!(planned.status, "SUCCEEDED");

    let confirm_result =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "operation.confirm",
                "op-confirm-console",
                &[("operation_id", "op-operation-lifecycle-console")],
            ));
    assert!(
        confirm_result.is_err(),
        "already applied operation should not be confirmable again"
    );

    let logs = OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .dispatch(request(
            "log.query",
            "op-logs-console",
            &[("operation_id", "op-operation-lifecycle-console")],
        ))
        .expect("operation logs");
    assert_eq!(logs.capability_status, ActionCapabilityStatus::Readonly);
    assert!(!logs.logs.is_empty());

    let rollback =
        OrchestratorActionDispatcher::with_endpoint_probe(&mut store, StaticEndpointProbe)
            .dispatch(request(
                "operation.rollback",
                "op-rollback-console",
                &[
                    ("operation_id", "op-operation-lifecycle-console"),
                    ("confirm", "true"),
                ],
            ))
            .expect("rollback");
    assert_eq!(rollback.status, "ROLLED_BACK");
    assert!(store.endpoint("127.0.0.1:18080:gateway").is_none());
}

#[test]
fn action_console_keeps_memory_store_changes_visible_after_refresh() {
    let root = repo_root();
    let mut console =
        OrchestratorActionConsole::load_with_database_url(root, None).expect("console");
    console
        .dispatch_with_static_probe(request(
            "endpoint.create",
            "op-console-endpoint",
            &[
                ("endpoint", "127.0.0.1:19000:gateway"),
                ("service_id", "gateway"),
                ("protocol", "http"),
            ],
        ))
        .expect("console dispatch");
    let view = console.view().expect("view");
    assert!(
        view.endpoints
            .iter()
            .any(|endpoint| endpoint.endpoint == "127.0.0.1:19000:gateway"),
        "Memory store must keep GUI/TUI action results visible for the session"
    );
    let context = console.context().expect("context");
    assert!(
        context
            .endpoints
            .iter()
            .any(|endpoint| endpoint.endpoint == "127.0.0.1:19000:gateway")
    );
}

#[test]
fn action_console_release_install_uses_loaded_release_manifest() {
    let root = repo_root();
    let mut console =
        OrchestratorActionConsole::load_with_database_url(root, None).expect("console");
    let result = console
        .dispatch_with_static_probe(request(
            "release.install",
            "op-console-release-install",
            &[("service_id", "gateway"), ("confirm", "true")],
        ))
        .expect("release install");
    assert_eq!(result.status, "SUCCEEDED");
    assert!(
        result
            .changed_objects
            .iter()
            .any(|object| object.starts_with("Route:")),
        "console release.install should use loaded release.yaml and record routes"
    );

    let view = console.view().expect("view");
    assert!(view.release_registry.iter().any(|row| {
        row.service_name == "gateway" && row.record_type == "route" && row.source == "store"
    }));

    let routes = console
        .dispatch_with_static_probe(request("route.list", "op-console-route-list", &[]))
        .expect("route list");
    assert_eq!(routes.status, "READONLY");
    assert!(
        routes
            .result
            .get("routes")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|route| route
                .get("target_service_name")
                .and_then(serde_json::Value::as_str)
                == Some("gateway"))),
        "route.list should read formal route registry rows"
    );
}

#[test]
fn orchestrator_database_migration_contains_only_formal_tables() {
    let root = repo_root();
    let sql = fs::read_to_string(
        root.join("services/orchestrator/migrations/000001_orchestrator_schema.up.sql"),
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
        sql.contains("CONSTRAINT service_endpoints_identity_shape")
            && sql.contains("endpoint = ip || ':' || port::TEXT || ':' || service_name")
            && sql.contains("CONSTRAINT service_endpoints_service_id_matches_identity"),
        "service_endpoints must enforce ip:port:service-name identity"
    );
    assert!(
        sql.contains("service_links_to_type CHECK (to_type IN ('endpoint', 'endpoint-group'))")
            && sql.contains(
                "service_routes_target_type CHECK (target_type IN ('endpoint', 'endpoint-group', 'frontend'))"
            ),
        "database routes and links must use endpoint-group instead of service-set"
    );
    assert!(
        !sql.contains("service-set") && !sql.contains("service_sets"),
        "formal orchestrator database schema must not retain service-set"
    );
    assert!(
        report.non_formal_tables.is_empty(),
        "non-formal tables should not appear: {:?}",
        report.non_formal_tables
    );
}

#[test]
fn compose_separates_orchestrator_and_service_databases() {
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
        "auth-db",
        "problem-db",
        "judge-db",
        "user-db",
        "orchestrator-migrations",
        "auth-service-migrations",
        "problem-service-migrations",
        "judge-api-migrations",
        "user-service-migrations",
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
        yaml_text(orchestrator_migrations)
            .contains("../../services/orchestrator/migrations:/migrations:ro"),
        "orchestrator migrations must mount only service-local migrations"
    );
    assert!(
        yaml_text(orchestrator_migrations).contains("ORCHESTRATOR_DATABASE_URL"),
        "orchestrator migrations must use ORCHESTRATOR_DATABASE_URL"
    );

    assert!(
        !services
            .keys()
            .any(|key| key.as_str() == Some("oj-migrations") || key.as_str() == Some("postgresql")),
        "compose must not use one centralized OJ database or migration job"
    );

    for (database_service, database_name, password_env) in [
        ("auth-db", "ojos_auth", "AUTH_POSTGRES_PASSWORD"),
        ("problem-db", "ojos_problem", "PROBLEM_POSTGRES_PASSWORD"),
        ("judge-db", "ojos_judge", "JUDGE_POSTGRES_PASSWORD"),
        ("user-db", "ojos_user", "USER_POSTGRES_PASSWORD"),
    ] {
        let service = services
            .get(serde_yaml::Value::String(database_service.to_string()))
            .unwrap_or_else(|| panic!("compose missing service {database_service}"));
        let text = yaml_text(service);
        assert!(
            text.contains(&format!("POSTGRES_DB: ${{")) && text.contains(database_name),
            "{database_service} must initialize its own service database {database_name}"
        );
        assert!(
            text.contains(password_env),
            "{database_service} must use a service-owned password environment variable"
        );
        assert!(
            text.contains(&format!("{database_service}-data:/var/lib/postgresql/data")),
            "{database_service} must use its own database volume"
        );
    }

    for (migration_service, mount, database_env, database_host) in [
        (
            "orchestrator-migrations",
            "../../services/orchestrator/migrations:/migrations:ro",
            "ORCHESTRATOR_DATABASE_URL",
            "orchestrator-db",
        ),
        (
            "auth-service-migrations",
            "../../services/auth-service/migrations:/migrations:ro",
            "AUTH_DATABASE_URL",
            "auth-db",
        ),
        (
            "problem-service-migrations",
            "../../services/problem-service/migrations:/migrations:ro",
            "PROBLEM_DATABASE_URL",
            "problem-db",
        ),
        (
            "judge-api-migrations",
            "../../services/judge-api/migrations:/migrations:ro",
            "JUDGE_DATABASE_URL",
            "judge-db",
        ),
        (
            "user-service-migrations",
            "../../services/user-service/migrations:/migrations:ro",
            "USER_DATABASE_URL",
            "user-db",
        ),
    ] {
        let service = services
            .get(serde_yaml::Value::String(migration_service.to_string()))
            .unwrap_or_else(|| panic!("compose missing service {migration_service}"));
        let text = yaml_text(service);
        assert!(
            text.contains(mount),
            "{migration_service} must mount only its service-local migrations"
        );
        assert!(
            text.contains(database_env),
            "{migration_service} must use its service-owned database URL"
        );
        assert!(
            text.contains(database_host),
            "{migration_service} must point at {database_host}"
        );
    }

    for (service_name, database_env) in [
        ("auth-service", "AUTH_DATABASE_URL"),
        ("problem-service", "PROBLEM_DATABASE_URL"),
        ("judge-api", "JUDGE_DATABASE_URL"),
        ("user-service", "USER_DATABASE_URL"),
    ] {
        let service = services
            .get(serde_yaml::Value::String(service_name.to_string()))
            .unwrap_or_else(|| panic!("compose missing service {service_name}"));
        let text = yaml_text(service);
        assert!(
            text.contains(database_env),
            "{service_name} must use its service-owned database URL"
        );
        assert!(
            !text.contains("OJ_DATABASE_URL"),
            "{service_name} must not use a shared OJ_DATABASE_URL"
        );
        assert!(
            !text.contains("ORCHESTRATOR_DATABASE_URL"),
            "{service_name} must not receive ORCHESTRATOR_DATABASE_URL"
        );
    }

    let gateway = services
        .get(serde_yaml::Value::String("gateway".to_string()))
        .expect("compose missing service gateway");
    let gateway_text = yaml_text(gateway);
    for database_env in [
        "AUTH_DATABASE_URL",
        "PROBLEM_DATABASE_URL",
        "JUDGE_DATABASE_URL",
        "USER_DATABASE_URL",
        "ORCHESTRATOR_DATABASE_URL",
        "OJ_DATABASE_URL",
    ] {
        assert!(
            !gateway_text.contains(database_env),
            "gateway must not receive service-owned database URL {database_env}"
        );
    }
    assert!(
        gateway_text.contains("AUTH_SERVICE_ENDPOINT"),
        "gateway must call auth-service API instead of reading the auth database"
    );

    assert!(
        !root
            .join("deploy/postgresql-init/001_service_databases.sql")
            .exists(),
        "service databases must be initialized by service-owned Postgres containers"
    );
}

#[test]
fn service_local_migrations_stay_inside_service_database_boundaries() {
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
        "'release.install'",
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
    let migration_roots = [
        (
            "auth-service",
            "services/auth-service/migrations",
            Vec::<&str>::new(),
        ),
        (
            "problem-service",
            "services/problem-service/migrations",
            vec!["references users", "from users", "join users"],
        ),
        (
            "judge-api",
            "services/judge-api/migrations",
            vec![
                "references users",
                "references problems",
                "references test_cases",
                "from problems",
                "join problems",
                "from users",
                "join users",
            ],
        ),
        (
            "user-service",
            "services/user-service/migrations",
            vec![
                "references users",
                "references problems",
                "references submissions",
                "from users",
                "join users",
                "from problems",
                "join problems",
                "from submissions",
                "join submissions",
            ],
        ),
    ];

    for (service_name, migration_root, forbidden_cross_service_patterns) in migration_roots {
        for entry in fs::read_dir(root.join(migration_root))
            .unwrap_or_else(|_| panic!("{service_name} migrations should exist"))
        {
            let entry = entry.expect("migration entry");
            if entry.path().extension().and_then(|value| value.to_str()) != Some("sql") {
                continue;
            }
            let sql = fs::read_to_string(entry.path()).expect("read service migration");
            let lowered = sql.to_lowercase();
            for item in &forbidden_table_patterns {
                assert!(
                    !lowered.contains(item),
                    "{} must not create or write orchestrator table pattern {item}",
                    entry
                        .file_name()
                        .to_str()
                        .expect("migration file name should be UTF-8")
                );
            }
            for item in forbidden_permission_patterns {
                assert!(
                    !lowered.contains(item),
                    "{} must not seed orchestrator or launcher permission {item}",
                    entry
                        .file_name()
                        .to_str()
                        .expect("migration file name should be UTF-8")
                );
            }
            for item in &forbidden_cross_service_patterns {
                assert!(
                    !lowered.contains(item),
                    "{} migration must not depend on cross-service database object pattern {item}",
                    service_name
                );
            }
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
        validate_service_manifest_file(&root, Path::new("services/problem-service/service.yaml"))
            .unwrap();
    store.put_service(gateway).expect("put service");
    store.put_service(problem_api).expect("put problem api");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
        .upsert_host_service(HostService {
            host_ip: "127.0.0.1".to_string(),
            service_name: "gateway".to_string(),
            version: "0.1.0".to_string(),
            status: "running".to_string(),
            config: serde_json::json!({}),
            labels: serde_json::json!({"source": "test"}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put host service");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
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
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8083:problem-service".to_string(),
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
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
        store.endpoints(),
        store.links(),
        Vec::new(),
        vec![LogView {
            source_id: "gateway:health".to_string(),
            service_id: "gateway".to_string(),
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
            summary: "Service observable".to_string(),
            operation_id: String::new(),
            data: serde_json::json!({}),
            findings: Vec::new(),
            created_at: String::new(),
        }],
    )
    .expect("topology");
    let operation = service_health_check_operation(
        "op-health-gateway",
        "gateway",
        Some("127.0.0.1:8080:gateway"),
    )
    .expect("health operation");
    store.put_operation(operation).expect("put operation");
    store
        .upsert_service_release(ServiceRelease {
            service_name: "gateway".to_string(),
            version: "0.1.0".to_string(),
            release_url: "local://services/gateway".to_string(),
            manifest: serde_json::json!({"service_name": "gateway"}),
            checksum: String::new(),
            created_at: String::new(),
        })
        .expect("put service release");
    store
        .upsert_service_route(ServiceRoute {
            path: "/api/problem/**".to_string(),
            method: "ANY".to_string(),
            target_type: "endpoint-group".to_string(),
            target_service_name: "problem-service".to_string(),
            target_selector: serde_json::json!({"group": "problem-service[*]"}),
            permission: "problem.problem.read".to_string(),
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put service route");
    store
        .upsert_service_migration_record(ServiceMigrationRecord {
            service_name: "problem-service".to_string(),
            migration_version: "0001".to_string(),
            checksum: String::new(),
            status: "registered".to_string(),
            applied_at: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put service migration");
    store
        .upsert_service_permission_record(ServicePermissionRecord {
            service_name: "problem-service".to_string(),
            permission_key: "problem.problem.read".to_string(),
            source: "release".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put service permission");
    store
        .upsert_service_frontend_entry(ServiceFrontendEntry {
            service_name: "problem-service".to_string(),
            enabled: true,
            route_prefix: "/problems".to_string(),
            remote_entry: "/assets/problem-service/remoteEntry.js".to_string(),
            menu_items: vec!["problems".to_string()],
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put service frontend");
    store
        .upsert_service_redis_resource(ServiceRedisResource {
            service_name: "problem-service".to_string(),
            name: "ojos:problem:{id}".to_string(),
            kind: "hash".to_string(),
            usage: "cache".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put service redis resource");
    store
        .upsert_service_storage_resource(ServiceStorageResource {
            service_name: "problem-service".to_string(),
            object_type: "testdata".to_string(),
            bucket: "problem-testdata".to_string(),
            path_prefix: "/problems".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put service storage resource");
    store
        .upsert_rendered_service_config(RenderedServiceConfig {
            service_name: "problem-service".to_string(),
            version: "0.1.0".to_string(),
            config: serde_json::json!({"service_name": "problem-service"}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put rendered config");
    store
        .upsert_node(NodeRecord {
            node_id: "root".to_string(),
            host_ip: "127.0.0.1".to_string(),
            parent_node_id: String::new(),
            role: "root".to_string(),
            labels: serde_json::json!({}),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put node");
    store
        .upsert_service_api_surface(ServiceApiSurface {
            service_name: "problem-service".to_string(),
            version: "0.1.0".to_string(),
            api_id: "problem.problem.read".to_string(),
            protocol: "http".to_string(),
            port_name: "http".to_string(),
            path_prefix: "/api/problem/problems".to_string(),
            methods: vec!["GET".to_string()],
            visibility: "descendants".to_string(),
            auth_mode: "user".to_string(),
            permission: "problem.problem.read".to_string(),
            stability: "stable".to_string(),
            api_version: "v1".to_string(),
            rate_limit: String::new(),
            timeout: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put api surface");
    store
        .upsert_deployed_service_api(DeployedServiceApi {
            host_ip: "127.0.0.1".to_string(),
            service_name: "problem-service".to_string(),
            version: "0.1.0".to_string(),
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            api_id: "problem.problem.read".to_string(),
            status: "running".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put deployed api");
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
        ("ServiceRelease", "service_releases"),
        ("HostService", "host_services"),
        ("Service", "services"),
        ("Endpoint", "service_endpoints"),
        ("Route", "service_routes"),
        ("MigrationRecord", "service_migration_records"),
        ("Permission", "service_permission_records"),
        ("Frontend", "service_frontend_entries"),
        ("RedisResource", "service_redis_resources"),
        ("StorageResource", "service_storage_resources"),
        ("RenderedConfig", "rendered_service_configs"),
        ("Node", "nodes"),
        ("ServiceApiSurface", "service_api_surfaces"),
        ("DeployedServiceApi", "deployed_service_apis"),
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
    assert!(
        !plan
            .writes
            .iter()
            .any(|write| write.object_type == "Set" || write.table == "service_sets"),
        "service-name endpoint groups are derived queries and must not be persisted"
    );
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
    assert!(
        store.statements().iter().all(|statement| {
            !statement.sql.contains("service-set") && !statement.sql.contains("service_sets")
        }),
        "PgOrchestratorStore statements must not persist formal service-set state"
    );
    let link_statement = store
        .statements()
        .iter()
        .find(|statement| statement.name == "service_links.upsert")
        .expect("link upsert statement should exist");
    assert!(
        link_statement.sql.contains("'endpoint'")
            && !link_statement.sql.contains("'service-set'")
            && !link_statement.sql.contains("'endpoint-group'"),
        "stored links must target concrete endpoints; endpoint groups stay derived"
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
fn pg_orchestrator_store_maps_operation_state_markers_to_db_time() {
    for marker in ["confirmed", "started", "finished", "failed", "rolled_back"] {
        assert_eq!(
            crate::database::db_time_text(marker),
            "now",
            "{marker} should become a database-side timestamp"
        );
    }
    assert_eq!(
        crate::database::db_time_text("session"),
        "",
        "session-style lock expiry should still use the DB default interval"
    );
    assert_eq!(
        crate::database::db_time_text("2026-06-29T00:00:00Z"),
        "2026-06-29T00:00:00Z"
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
        .expect_err("plan-only service lifecycle should fail without explicit execution");
    let logs = store.operation_logs("op-lock-log");
    assert!(
        logs.iter().any(|record| !record.step_id.is_empty()),
        "apply should persist step logs"
    );
    let driver_log = logs
        .iter()
        .find(|record| record.step_id == "driver:service.restart")
        .expect("service lifecycle should persist driver result log");
    assert_eq!(
        driver_log
            .data
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("PLANNED")
    );
    assert_eq!(
        driver_log
            .data
            .get("command")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(serde_json::Value::as_str),
        Some("docker")
    );
    assert_eq!(
        store.operation("op-lock-log").map(|item| &item.status),
        Some(&OperationStatus::Failed)
    );
    assert!(
        store
            .acquire_operation_lock(OperationLock {
                lock_key: "operation:op-lock-log".to_string(),
                operation_id: "op-lock-log".to_string(),
                owner: "test".to_string(),
                expires_at: "session".to_string(),
                created_at: String::new(),
            })
            .expect("lock can be acquired after failed apply"),
        "apply failure must release operation lock"
    );
}

#[test]
fn local_process_lifecycle_failure_is_persisted_by_executor() {
    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "orchestrator".to_string();
    service.runtime.mode = RuntimeMode::LocalProcess;
    service.runtime.driver = "node".to_string();
    store.put_service(service).expect("put service");
    let operation =
        service_lifecycle_operation("op-local-start", "service.start", "orchestrator").unwrap();
    store.put_operation(operation).expect("put operation");

    let failed = OperationExecutor::new(&mut store)
        .apply("op-local-start")
        .expect_err("local process lifecycle requires release runtime");
    assert!(
        failed
            .to_string()
            .contains("requires release runtime configuration")
    );
    let stored = store.operation("op-local-start").expect("stored operation");
    assert_eq!(stored.status, OperationStatus::Failed);
    assert!(
        stored
            .error_message
            .contains("requires release runtime configuration")
    );
    assert!(
        store
            .operation_logs("op-local-start")
            .iter()
            .any(|record| record.level == "error"
                && record.message.contains("operation service.start failed")),
        "unsupported lifecycle should be visible in operation logs"
    );
}

#[test]
fn fixed_executor_drivers_reject_arbitrary_actions() {
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
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

    let link = Link {
        source_endpoint: "127.0.0.1:8080:gateway".to_string(),
        target_endpoint: "127.0.0.1:8081:problem-service".to_string(),
        protocol: "http".to_string(),
        auth_mode: "none".to_string(),
        scope: "internal".to_string(),
        health: "unknown".to_string(),
        latency_ms: None,
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let link_request = DriverRequest {
        action: "link.create".to_string(),
        service_id: endpoint.service_id.clone(),
        endpoint: endpoint.endpoint.clone(),
        link: Some(link),
        log_source: None,
        release_runtime: None,
    };
    assert_eq!(
        external
            .execute(&link_request)
            .expect("external link metadata action")
            .status,
        "SUPPORTED"
    );

    let missing_link_request = DriverRequest {
        action: "link.update".to_string(),
        service_id: endpoint.service_id.clone(),
        endpoint: endpoint.endpoint.clone(),
        link: None,
        log_source: None,
        release_runtime: None,
    };
    assert!(
        external.execute(&missing_link_request).is_err(),
        "link metadata actions must carry source_endpoint and target_endpoint"
    );

    let diagnostics_export = DriverRequest {
        action: "diagnostic.export".to_string(),
        service_id: String::new(),
        endpoint: String::new(),
        link: None,
        log_source: None,
        release_runtime: None,
    };
    assert_eq!(
        external
            .execute(&diagnostics_export)
            .expect("external diagnostics export action")
            .status,
        "SUPPORTED"
    );
}

#[test]
fn executor_rejects_arbitrary_shell() {
    let compose = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
    for action in [
        "service.shell",
        "service.exec",
        "script.run",
        "powershell.run",
        "bash.run",
    ] {
        let err = compose
            .command_for(action, "gateway")
            .expect_err("fixed docker compose driver must reject arbitrary action");
        assert!(
            err.to_string().contains("not fixed"),
            "{action} should be rejected as non-fixed command"
        );
    }
    let unsafe_binary = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml")
        .with_docker_binary_for_test("docker.exe /c calc");
    assert!(
        unsafe_binary
            .command_for("service.start", "gateway")
            .is_err(),
        "driver executable must be a single safe binary name"
    );
}

#[test]
fn docker_compose_driver_builds_allowed_commands() {
    let compose = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
    let cases = [
        ("release.install", "up", true),
        ("service.enable", "up", true),
        ("service.start", "start", false),
        ("service.stop", "stop", false),
        ("service.restart", "restart", false),
        ("service.delete", "rm", false),
        ("log.create", "logs", false),
        ("service.health.check", "ps", false),
    ];
    for (action, subcommand, detached) in cases {
        let command = compose
            .command_for(action, "gateway")
            .expect("allowed docker compose command");
        assert_eq!(command[0], "docker");
        assert!(command.contains(&subcommand.to_string()));
        if detached {
            assert!(command.contains(&"-d".to_string()));
        }
        if subcommand != "ps" {
            assert_eq!(command.last().map(String::as_str), Some("gateway"));
        }
    }
}

#[test]
fn docker_compose_driver_rejects_unknown_action() {
    let compose = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml");
    let err = compose
        .command_for("endpoint.create", "gateway")
        .expect_err("docker compose driver must reject endpoint metadata actions");
    assert!(
        err.to_string()
            .contains("docker compose driver action is not fixed")
    );
}

#[test]
fn local_process_driver_reports_unsupported_safely() {
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
        service_id: "orchestrator".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Orchestrator".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let start = driver_request_for_endpoint("service.start", &endpoint);
    let err = LocalProcessDriver::new()
        .execute(&start)
        .expect_err("local process start requires release runtime");
    assert!(
        err.to_string()
            .contains("requires release runtime configuration")
    );

    let logs = driver_request_for_endpoint("log.create", &endpoint);
    assert_eq!(
        LocalProcessDriver::new()
            .execute(&logs)
            .expect("read-only logs action")
            .status,
        "SUPPORTED"
    );
}

#[cfg(windows)]
fn free_local_tcp_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local test port");
    listener.local_addr().expect("local addr").port()
}

#[cfg(windows)]
fn assert_endpoint_unreachable(host: &str, port: u16) {
    for _ in 0..30 {
        if TcpStream::connect((host, port)).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("endpoint {host}:{port} should be unreachable after rollback");
}

#[cfg(windows)]
#[test]
fn release_install_local_process_starts_service_and_rollback_stops_it() {
    let _guard = DOCKER_BINARY_ENV_LOCK.lock().expect("env lock");
    let previous_root = std::env::var("ORCHESTRATOR_RELEASE_PACKAGE_ROOT").ok();
    let previous_state = std::env::var("OJOS_LOCAL_PROCESS_STATE_DIR").ok();
    let dir = tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    unsafe {
        std::env::set_var("ORCHESTRATOR_RELEASE_PACKAGE_ROOT", dir.path());
        std::env::set_var("OJOS_LOCAL_PROCESS_STATE_DIR", &state_dir);
    }

    let port = free_local_tcp_port();
    let endpoint_id = format!("127.0.0.1:{port}:local-demo");
    let mut service = valid_service();
    service.id = "local-demo".to_string();
    service.name = "Local Demo".to_string();
    service.kind = "backend-api".to_string();
    service.endpoint.default_port = port;
    service.endpoint.health_path = "/health".to_string();
    service.runtime.mode = RuntimeMode::LocalProcess;
    service.runtime.driver = "local-process".to_string();
    let script = format!(
        "$listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Parse('127.0.0.1'),{port});$listener.Start();while($true){{$client=$listener.AcceptTcpClient();$stream=$client.GetStream();$buffer=New-Object byte[] 1024;$null=$stream.Read($buffer,0,$buffer.Length);$bytes=[Text.Encoding]::ASCII.GetBytes(\"HTTP/1.1 200 OK`r`nContent-Length:2`r`n`r`nok\");$stream.Write($bytes,0,$bytes.Length);$client.Close();}}"
    );
    let release = ServiceReleaseManifest {
        schema_version: 1,
        service_name: service.id.clone(),
        version: service.version.clone(),
        description: "Local process demo release".to_string(),
        service_type: service.kind.clone(),
        source: ReleaseSourceDecl {
            kind: "local".to_string(),
            url: "local://local-demo".to_string(),
            checksum: String::new(),
        },
        runtime: ReleaseRuntimeDecl {
            kind: "local-process".to_string(),
            image: String::new(),
            binary: String::new(),
            system_service: String::new(),
            command: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-ExecutionPolicy".to_string(),
                "Bypass".to_string(),
                "-Command".to_string(),
                script,
            ],
            working_dir: String::new(),
            env: BTreeMap::new(),
        },
        frontend: ReleaseFrontendDecl::default(),
        backend: ReleaseBackendDecl {
            protocol: "http".to_string(),
            port,
            health_path: "/health".to_string(),
        },
        migrations: Vec::new(),
        permissions: Vec::new(),
        routes: Vec::new(),
        apis: Vec::new(),
        redis: Vec::new(),
        storage: Vec::new(),
        dependencies: Vec::new(),
        required_apis: Vec::new(),
        service_identity: ReleaseServiceIdentityDecl::default(),
        config_schema: serde_json::json!({}),
        secrets: Vec::new(),
        observability: ReleaseObservabilityDecl::default(),
    };
    validate_service_release(&release).expect("local-process release validates");
    let operation = release_install_operation_with_release(
        "op-local-process-release-install",
        &service,
        Some(&release),
        &[],
        "127.0.0.1",
        Some(&endpoint_id),
        serde_json::json!({"execute_service_driver": true}),
    )
    .and_then(|operation| confirm_operation(&operation))
    .expect("confirmed local process release install");
    let mut store = MemoryOrchestratorStore::new();
    store.put_operation(operation).expect("put operation");
    let applied = OperationExecutor::with_endpoint_probe(
        &mut store,
        TcpEndpointProbe::new(Duration::from_millis(200)),
    )
    .with_service_driver_execution_enabled()
    .apply("op-local-process-release-install")
    .expect("local process release install should start and pass health");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    let host = store.host_services()[0].clone();
    assert_eq!(host.status, "running");
    assert!(
        host.config
            .get("external_steps")
            .and_then(|value| value.get("driver"))
            .and_then(|value| value.get("pid"))
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|pid| pid > 0),
        "rendered runtime config should record local process pid"
    );
    assert!(
        state_dir.join("local-demo.pid").exists(),
        "local process pid file should be recorded"
    );

    store
        .operation_logs("op-local-process-release-install")
        .iter()
        .find(|record| record.step_id == "driver:release.install")
        .and_then(|record| record.data.get("pid"))
        .and_then(serde_json::Value::as_u64)
        .filter(|pid| *pid > 0)
        .expect("driver log should record pid");

    OperationExecutor::new(&mut store)
        .rollback("op-local-process-release-install")
        .expect("rollback should stop local process");
    assert!(
        !state_dir.join("local-demo.pid").exists(),
        "rollback should remove local process pid file"
    );
    assert_endpoint_unreachable("127.0.0.1", port);

    unsafe {
        match previous_root {
            Some(value) => std::env::set_var("ORCHESTRATOR_RELEASE_PACKAGE_ROOT", value),
            None => std::env::remove_var("ORCHESTRATOR_RELEASE_PACKAGE_ROOT"),
        }
        match previous_state {
            Some(value) => std::env::set_var("OJOS_LOCAL_PROCESS_STATE_DIR", value),
            None => std::env::remove_var("OJOS_LOCAL_PROCESS_STATE_DIR"),
        }
    }
}

#[test]
fn external_endpoint_driver_does_not_start_services() {
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
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
    let start = driver_request_for_endpoint("service.start", &endpoint);
    let err = ExternalEndpointDriver
        .execute(&start)
        .expect_err("external endpoint driver must not start services");
    assert!(err.to_string().contains("cannot control service lifecycle"));

    let register = driver_request_for_endpoint("endpoint.create", &endpoint);
    assert_eq!(
        ExternalEndpointDriver
            .execute(&register)
            .expect("external endpoint metadata action")
            .status,
        "SUPPORTED"
    );
}

#[test]
fn unsupported_driver_action_writes_operation_log() {
    let mut store = MemoryOrchestratorStore::new();
    let mut service = valid_service();
    service.id = "orchestrator".to_string();
    service.runtime.mode = RuntimeMode::LocalProcess;
    service.runtime.driver = "local-process".to_string();
    store.put_service(service).expect("put service");
    let operation =
        service_lifecycle_operation("op-unsupported-driver", "service.start", "orchestrator")
            .expect("lifecycle operation");
    store.put_operation(operation).expect("put operation");

    let err = OperationExecutor::new(&mut store)
        .apply("op-unsupported-driver")
        .expect_err("unsupported driver action should fail operation");
    assert!(
        err.to_string()
            .contains("requires release runtime configuration")
    );
    assert_eq!(
        store
            .operation("op-unsupported-driver")
            .map(|operation| &operation.status),
        Some(&OperationStatus::Failed)
    );
    assert!(
        store
            .operation_logs("op-unsupported-driver")
            .iter()
            .any(|record| record.level == "error"
                && record.message.contains("operation service.start failed")),
        "unsupported driver action must be recorded as an operation log"
    );
}

#[test]
fn docker_compose_driver_runs_only_when_explicitly_enabled() {
    let endpoint = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
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
    let request = driver_request_for_endpoint("service.health.check", &endpoint);
    let plan_only = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml")
        .execute(&request)
        .expect("plan-only docker compose driver");
    assert_eq!(plan_only.status, "PLANNED");

    let missing_binary = DockerComposeDriver::new(".", "deploy/compose/docker-compose.yml")
        .with_docker_binary_for_test("ojos-docker-compose-missing")
        .with_execution_enabled()
        .execute(&request)
        .expect("explicit execution should return structured driver failure");
    assert_eq!(missing_binary.status, "FAILED");
    assert!(
        missing_binary
            .message
            .contains("docker compose fixed command failed to start")
    );
}

#[test]
fn driver_output_decoder_preserves_utf8_text() {
    assert_eq!(
        crate::executor::decode_driver_output_bytes("service started".as_bytes())
            .expect("UTF-8 output should decode"),
        "service started"
    );
}

#[test]
fn driver_output_decoder_rejects_non_utf8_text() {
    let err = crate::executor::decode_driver_output_bytes(&[0xff, 0xfe, 0xfd])
        .expect_err("driver output must be valid UTF-8");
    assert!(err.to_string().contains("driver output is not UTF-8"));
}

#[test]
fn orchestrator_code_forbids_lossy_text_decoding() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("orchestrator root")
        .to_path_buf();
    let mut offenders = Vec::new();
    collect_lossy_decoding_markers(&root, &root, &mut offenders);
    assert!(
        offenders.is_empty(),
        "orchestrator user-visible text must be strict UTF-8 with no lossy or non-UTF-8 decoding in {offenders:?}"
    );
}

fn collect_lossy_decoding_markers(root: &Path, dir: &Path, offenders: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read orchestrator source directory") {
        let entry = entry.expect("source entry");
        let path = entry.path();
        if path.is_dir() {
            collect_lossy_decoding_markers(root, &path, offenders);
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("source file must be UTF-8");
        let forbidden_markers = [
            ["from_utf8_", "lossy"].concat(),
            ["to_string_", "lossy"].concat(),
            ["encoding", "_rs"].concat(),
            ["G", "BK"].concat(),
            ["A", "NSI"].concat(),
            ["ch", "cp"].concat(),
        ];
        if forbidden_markers
            .iter()
            .any(|marker| source.contains(marker))
        {
            offenders.push(
                path.strip_prefix(&root)
                    .expect("source file should stay below root")
                    .to_str()
                    .expect("source path must be UTF-8")
                    .replace('\\', "/"),
            );
        }
    }
}

#[test]
fn endpoint_and_link_health_checks_return_formal_statuses() {
    let source = Endpoint {
        endpoint: "127.0.0.1:8080:gateway".to_string(),
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
        endpoint: "127.0.0.1:8083:problem-service".to_string(),
        service_id: "problem-service".to_string(),
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
fn operation_executor_persists_probed_endpoint_health() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:18080:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
            reachable: false,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
    let operation = endpoint_health_check_operation("op-probe-endpoint", "127.0.0.1:18080:gateway")
        .expect("endpoint health operation");
    store.put_operation(operation).expect("put operation");

    let applied = OperationExecutor::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .apply("op-probe-endpoint")
        .expect("apply endpoint health operation");
    assert_eq!(applied.status, OperationStatus::Succeeded);
    let endpoint = store
        .endpoint("127.0.0.1:18080:gateway")
        .expect("endpoint should exist");
    assert_eq!(endpoint.health, "unreachable");
    assert!(!endpoint.reachable);
    assert!(
        store
            .operation_logs("op-probe-endpoint")
            .iter()
            .any(
                |record| record.step_id == "health:endpoint:127.0.0.1:18080:gateway"
                    && record.level == "warn"
                    && record
                        .data
                        .get("reachable")
                        .and_then(serde_json::Value::as_bool)
                        == Some(false)
            ),
        "endpoint health apply should persist probed health in operation logs"
    );
}

#[test]
fn operation_executor_persists_link_health_from_target_probe() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    let mut problem_api = valid_service();
    problem_api.id = "problem-service".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    let source = Endpoint {
        endpoint: "127.0.0.1:18080:gateway".to_string(),
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
    };
    let target = Endpoint {
        endpoint: "127.0.0.1:18081:problem-service".to_string(),
        service_id: "problem-service".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "healthy".to_string(),
        reachable: false,
        display_name: "Problem API".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    store.put_endpoint(source.clone()).expect("put source");
    store.put_endpoint(target.clone()).expect("put target");
    let link = Link {
        source_endpoint: source.endpoint.clone(),
        target_endpoint: target.endpoint.clone(),
        protocol: "http".to_string(),
        auth_mode: "internal".to_string(),
        scope: "api".to_string(),
        health: "healthy".to_string(),
        latency_ms: Some(1),
        config_ref: String::new(),
        secret_ref: String::new(),
        policy: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    store.put_link(link.clone()).expect("put link");
    let operation =
        link_health_check_operation("op-probe-link", &link).expect("link health operation");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_endpoint_probe(&mut store, StaticEndpointProbe)
        .apply("op-probe-link")
        .expect("apply link health operation");
    let stored_link = store
        .get_link(&link.source_endpoint, &link.target_endpoint)
        .expect("get link")
        .expect("link should exist");
    assert_eq!(stored_link.health, "unreachable");
    assert_eq!(stored_link.latency_ms, None);
    assert_eq!(
        store
            .endpoint(&target.endpoint)
            .map(|endpoint| endpoint.health.as_str()),
        Some("unreachable")
    );
    assert!(
        store
            .operation_logs("op-probe-link")
            .iter()
            .any(|record| record.step_id
                == "health:link:127.0.0.1:18080:gateway>127.0.0.1:18081:problem-service"
                && record
                    .data
                    .get("health")
                    .and_then(serde_json::Value::as_str)
                    == Some("unreachable")),
        "link health apply should persist computed link health in operation logs"
    );
}

#[test]
fn endpoint_http_health_updates_store() {
    let endpoint = local_http_endpoint(
        "/health",
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
    );
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_endpoint(endpoint.clone()).expect("put endpoint");
    let operation = endpoint_health_check_operation("op-http-health", &endpoint.endpoint)
        .expect("endpoint health operation");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_endpoint_probe(
        &mut store,
        TcpEndpointProbe::new(Duration::from_secs(2)),
    )
    .apply("op-http-health")
    .expect("apply http health operation");
    let stored = store
        .endpoint(&endpoint.endpoint)
        .expect("endpoint should be stored");
    assert_eq!(stored.health, "healthy");
    assert!(stored.reachable);
    assert!(
        store
            .operation_logs("op-http-health")
            .iter()
            .any(
                |record| record.step_id == format!("health:endpoint:{}", endpoint.endpoint)
                    && record
                        .data
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        == Some("http 204")
            ),
        "http health_path check should be logged"
    );
}

#[test]
fn endpoint_tcp_health_updates_store() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local tcp listener");
    let socket_addr = listener.local_addr().expect("local addr").to_string();
    let endpoint_id = format!("{socket_addr}:gateway");
    thread::spawn(move || {
        let _ = listener.accept();
    });
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: endpoint_id.clone(),
            service_id: "gateway".to_string(),
            protocol: "tcp".to_string(),
            health_path: String::new(),
            health: "unknown".to_string(),
            reachable: false,
            display_name: "Local TCP".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
    let operation = endpoint_health_check_operation("op-tcp-health", &endpoint_id)
        .expect("endpoint health operation");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_endpoint_probe(
        &mut store,
        TcpEndpointProbe::new(Duration::from_secs(2)),
    )
    .apply("op-tcp-health")
    .expect("apply tcp health operation");
    let stored = store
        .endpoint(&endpoint_id)
        .expect("endpoint should be stored");
    assert_eq!(stored.health, "healthy");
    assert!(stored.reachable);
}

#[test]
fn endpoint_unreachable_is_recorded() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:9:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "tcp".to_string(),
            health_path: String::new(),
            health: "healthy".to_string(),
            reachable: true,
            display_name: "Closed TCP".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
    let operation = endpoint_health_check_operation("op-unreachable-health", "127.0.0.1:9:gateway")
        .expect("endpoint health operation");
    store.put_operation(operation).expect("put operation");

    OperationExecutor::with_endpoint_probe(
        &mut store,
        TcpEndpointProbe::new(Duration::from_millis(200)),
    )
    .apply("op-unreachable-health")
    .expect("apply unreachable health operation");
    let stored = store
        .endpoint("127.0.0.1:9:gateway")
        .expect("endpoint should be stored");
    assert_eq!(stored.health, "unreachable");
    assert!(!stored.reachable);
    assert!(
        store
            .operation_logs("op-unreachable-health")
            .iter()
            .any(|record| record.level == "warn"
                && record
                    .data
                    .get("reachable")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)),
        "unreachable health should be recorded as warn log"
    );
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
fn tcp_probe_uses_ip_port_only_for_http_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local http listener");
    let socket_addr = listener.local_addr().expect("local addr").to_string();
    let endpoint = Endpoint {
        endpoint: format!("{socket_addr}:gateway"),
        service_id: "gateway".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: "Local HTTP".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let captured_request = std::sync::Arc::new(Mutex::new(String::new()));
    let captured_for_thread = captured_request.clone();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept health request");
        let mut buffer = [0_u8; 1024];
        let bytes = stream.read(&mut buffer).expect("read health request");
        *captured_for_thread.lock().expect("lock captured request") =
            String::from_utf8(buffer[..bytes].to_vec()).expect("health request must be UTF-8");
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .expect("write response");
    });

    let result =
        check_endpoint_health_with_probe(&endpoint, &TcpEndpointProbe::new(Duration::from_secs(2)))
            .expect("http health check");
    assert_eq!(result.health, "healthy");
    let captured = captured_request
        .lock()
        .expect("lock captured request")
        .clone();
    assert!(
        captured.starts_with("GET /health "),
        "health request must use only the health path"
    );
    assert!(
        captured.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("host") && value.trim() == socket_addr.to_string()
            })
        }),
        "Host header must be ip:port, not ip:port:service-name"
    );
    assert!(
        !captured.contains("gateway\r\n"),
        "service-name must not be sent as part of the socket host"
    );
}

#[test]
fn link_health_requires_existing_endpoints() {
    let source = Endpoint {
        endpoint: "127.0.0.1:18100:gateway".to_string(),
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
    };
    let link = Link {
        source_endpoint: source.endpoint.clone(),
        target_endpoint: "127.0.0.1:18101:problem-service".to_string(),
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
    let target_health = EndpointHealthResult {
        endpoint: link.target_endpoint.clone(),
        health: "unreachable".to_string(),
        reachable: false,
        latency_ms: None,
        message: "target missing".to_string(),
    };
    let missing_target = check_link_health(&link, std::slice::from_ref(&source), &target_health)
        .expect("link health");
    assert_eq!(missing_target.health, "blocked");
    assert_eq!(missing_target.message, "target endpoint is missing");

    let missing_source = check_link_health(&link, &[], &target_health).expect("link health");
    assert_eq!(missing_source.health, "blocked");
    assert_eq!(missing_source.message, "source endpoint is missing");
}

#[test]
fn link_health_uses_target_reachability() {
    let source = Endpoint {
        endpoint: "127.0.0.1:18110:gateway".to_string(),
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
    };
    let target = Endpoint {
        endpoint: "127.0.0.1:18111:problem-service".to_string(),
        service_id: "problem-service".to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unreachable".to_string(),
        reachable: false,
        display_name: "Problem API".to_string(),
        note: String::new(),
        config: serde_json::json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    };
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
    let target_health = EndpointHealthResult {
        endpoint: target.endpoint.clone(),
        health: "unreachable".to_string(),
        reachable: false,
        latency_ms: None,
        message: "connection refused".to_string(),
    };
    let result = check_link_health(&link, &[source, target], &target_health).expect("link health");
    assert_eq!(result.health, "unreachable");
    assert_eq!(result.message, "target endpoint is unreachable");
}

#[test]
fn topology_reflects_endpoint_link_health() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    let mut problem_api = valid_service();
    problem_api.id = "problem-service".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:18120:gateway".to_string(),
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
        .expect("put source");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:18121:problem-service".to_string(),
            service_id: "problem-service".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "unreachable".to_string(),
            reachable: false,
            display_name: "Problem API".to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put target");
    store
        .put_link(Link {
            source_endpoint: "127.0.0.1:18120:gateway".to_string(),
            target_endpoint: "127.0.0.1:18121:problem-service".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "api".to_string(),
            health: "unreachable".to_string(),
            latency_ms: None,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put link");
    let topology = store.build_topology_view().expect("topology");
    assert_eq!(
        topology
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint == "127.0.0.1:18121:problem-service")
            .map(|endpoint| (endpoint.health.as_str(), endpoint.reachable)),
        Some(("unreachable", false))
    );
    assert_eq!(
        topology
            .links
            .iter()
            .find(|link| link.target_endpoint == "127.0.0.1:18121:problem-service")
            .map(|link| link.health.as_str()),
        Some("unreachable")
    );
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
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
        service_health_check_operation("op-store-view", "gateway", Some("127.0.0.1:8080:gateway"))
            .expect("operation");
    let operation = start_operation(&operation)
        .and_then(|operation| fail_operation(&operation, "health check failed"))
        .expect("failed operation");
    store.put_operation(operation).expect("operation");
    store
        .append_operation_log(operation_step_log_record(
            "op-store-view",
            "probe_service_health",
            "error",
            "health check failed",
            serde_json::json!({"endpoint": "127.0.0.1:8080:gateway"}),
        ))
        .expect("operation log");
    let view = load_orchestrator_view_from_store(schemas, &store).expect("store view");
    assert_eq!(view.services.len(), 1);
    assert_eq!(view.endpoints[0].endpoint, "127.0.0.1:8080:gateway");
    assert_eq!(view.operations[0].action, "service.health.check");
    assert_eq!(view.operations[0].operation_id, "op-store-view");
    assert_eq!(view.operations[0].status, "FAILED");
    assert_eq!(view.operations[0].error, "health check failed");
    assert_eq!(view.operations[0].log_count, 1);
    assert_eq!(view.operations[0].created_at, "planned");
    assert_eq!(view.operations[0].updated_at, "failed");
    assert!(
        view.logs
            .iter()
            .any(|log| log.operation_id == "op-store-view"
                && log.level == "error"
                && log.message == "health check failed")
    );
}

#[test]
fn operation_workbench_session_merges_into_view_operations_and_logs() {
    let root = repo_root();
    let context = crate::workbench::load_operation_workbench_context_with_database_url(&root, None)
        .expect("workbench context")
        .with_memory_store();
    let mut view = load_orchestrator_view_with_database_url(&root, None).expect("repo view");
    let session = context
        .build_session("release.install")
        .and_then(|session| context.confirm(&session))
        .and_then(|session| context.apply(&session))
        .expect("applied session");

    merge_operation_workbench_session_into_view(&mut view, &session);

    let row = view
        .operations
        .iter()
        .find(|row| row.operation_id == session.current_operation.operation_id)
        .expect("merged operation row");
    assert_eq!(row.status, "SUCCEEDED");
    assert_eq!(row.result, "SUCCEEDED");
    assert_eq!(row.log_count, session.logs.len());
    assert!(
        view.logs.iter().any(
            |log| log.operation_id == session.current_operation.operation_id
                && log.path.starts_with("step:")
        ),
        "merged view should expose current operation logs"
    );
    assert_eq!(
        view.operation_workbench
            .as_ref()
            .map(|workbench| workbench.result_status.as_str()),
        Some("SUCCEEDED")
    );
}

#[test]
fn diagnostic_report_json_exports_observable_summary() {
    let topology = build_topology(
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string()],
        vec![Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
    assert!(report.contains("service_name_endpoint_groups_summary"));
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
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
            serde_json::json!({"endpoint": "127.0.0.1:8080:gateway"}),
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
            .get("service_name_endpoint_groups_summary")
            .and_then(|summary| summary.get("count"))
            .and_then(serde_json::Value::as_u64)
            .is_some(),
        "DiagnosticReport should include derived service-name endpoint groups"
    );
    assert!(
        report
            .data
            .get("recent_operation_logs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|item| item.get("operation_id")
                == Some(&serde_json::Value::String("op-failed".to_string()))))
    );
    assert!(
        report
            .data
            .get("action_matrix")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items.iter().any(|item| {
                item.get("action_id").and_then(serde_json::Value::as_str) == Some("endpoint.create")
                    && item
                        .get("capability_status")
                        .and_then(serde_json::Value::as_str)
                        == Some("STORE_BACKED")
            })),
        "DiagnosticReport should include action matrix evidence"
    );
    assert!(
        report
            .data
            .get("unsupported_capabilities")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str() == Some("service.start"))),
        "DiagnosticReport should list unsupported capabilities honestly"
    );

    let json_export = export_diagnostic_report(&report, "json").expect("json export");
    assert!(json_export.content.contains("diag-current"));
    let markdown_export = export_diagnostic_report(&report, "markdown").expect("markdown export");
    assert!(
        markdown_export
            .content
            .contains("# DiagnosticReport diag-current")
    );
    assert!(markdown_export.content.contains("- services: 1"));
    assert!(
        markdown_export
            .content
            .contains("- endpoints: 1 unhealthy: 1")
    );
    assert!(markdown_export.content.contains("## Evidence"));
    assert!(markdown_export.content.contains("recent_operation_logs"));
    assert!(markdown_export.content.contains("action_matrix"));
    assert!(markdown_export.content.contains("unsupported_capabilities"));
    assert!(markdown_export.content.contains("database_schema_check"));
    assert!(
        markdown_export
            .content
            .contains("forbidden_concept_scan_summary")
    );
    assert!(export_diagnostic_report(&report, "html").is_err());
}

#[test]
fn reconcile_tick_refreshes_health_topology_and_diagnostics() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    let mut problem_api = valid_service();
    problem_api.id = "problem-service".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
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
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8083:problem-service".to_string(),
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
            .endpoint("127.0.0.1:8080:gateway")
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
    problem_api.id = "problem-service".to_string();
    store.put_service(gateway).expect("put gateway");
    store.put_service(problem_api).expect("put problem api");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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
            endpoint: "127.0.0.1:8083:problem-service".to_string(),
            service_id: "problem-service".to_string(),
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
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8083:problem-service".to_string(),
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
        "127.0.0.1:8080:gateway".to_string(),
        vec!["gateway".to_string(), "problem-service".to_string()],
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

#[test]
fn reconcile_loop_runs_bounded_ticks_and_can_stop() {
    let mut store = MemoryOrchestratorStore::new();
    let mut gateway = valid_service();
    gateway.id = "gateway".to_string();
    store.put_service(gateway).expect("put gateway");
    store
        .put_endpoint(Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
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

    let loop_result = run_reconcile_loop(
        &mut store,
        &StaticEndpointProbe,
        ReconcileLoopConfig::bounded("daemon-core", 3),
        |tick_index, tick| tick_index == 2 && tick.topology_snapshot_id.is_some(),
    )
    .expect("bounded reconcile loop");

    assert_eq!(loop_result.loop_id, "daemon-core");
    assert!(loop_result.stopped);
    assert_eq!(loop_result.ticks.len(), 2);
    assert_eq!(
        loop_result.ticks[0].topology_snapshot_id.as_deref(),
        Some("tick-daemon-core-1")
    );
    assert_eq!(
        loop_result.ticks[1].diagnostic_report_id.as_deref(),
        Some("diag-tick-daemon-core-2")
    );
    assert!(
        store
            .diagnostic_reports()
            .iter()
            .any(|report| report.report_id == "diag-tick-daemon-core-2"),
        "loop should persist diagnostics through the same Store path as a daemon tick"
    );
}

#[test]
fn node_tree_resolves_ancestors_descendants_and_rejects_cycles() {
    let mut store = MemoryOrchestratorStore::new();
    put_test_node(&mut store, "root", "10.0.0.1", "", "root");
    put_test_node(&mut store, "child", "10.0.0.2", "root", "node");
    put_test_node(&mut store, "grandchild", "10.0.0.3", "child", "node");
    put_test_node(&mut store, "sibling", "10.0.0.4", "root", "node");
    put_test_node(&mut store, "standalone", "10.0.0.5", "", "standalone");

    assert!(
        store
            .ancestors_of("root")
            .expect("root ancestors")
            .is_empty()
    );
    assert_eq!(
        store
            .ancestors_of("child")
            .expect("child ancestors")
            .into_iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        vec!["root"]
    );
    assert_eq!(
        store
            .ancestors_of("grandchild")
            .expect("grandchild ancestors")
            .into_iter()
            .map(|node| node.node_id)
            .collect::<Vec<_>>(),
        vec!["child", "root"]
    );
    assert!(
        !store
            .ancestors_of("grandchild")
            .expect("grandchild ancestors")
            .iter()
            .any(|node| node.node_id == "sibling")
    );
    assert!(
        store
            .ancestors_of("standalone")
            .expect("standalone ancestors")
            .is_empty()
    );
    assert!(
        store
            .upsert_node(test_node("orphan", "10.0.0.6", "missing", "node"))
            .expect_err("missing parent should fail")
            .to_string()
            .contains("parent node missing not found")
    );

    let mut cyclic = MemoryOrchestratorStore::new();
    put_test_node(&mut cyclic, "root", "10.0.1.1", "", "root");
    put_test_node(&mut cyclic, "a", "10.0.1.2", "root", "node");
    put_test_node(&mut cyclic, "b", "10.0.1.3", "a", "node");
    assert!(
        cyclic
            .upsert_node(test_node("a", "10.0.1.2", "b", "node"))
            .expect_err("cycle should fail")
            .to_string()
            .contains("cycle")
    );
}

#[test]
fn effective_api_view_exposes_running_ancestor_descendant_apis_only() {
    let mut store = MemoryOrchestratorStore::new();
    put_test_node(&mut store, "root", "10.1.0.1", "", "root");
    put_test_node(&mut store, "child", "10.1.0.2", "root", "node");
    put_test_node(&mut store, "sibling", "10.1.0.3", "root", "node");
    put_test_node(&mut store, "grandchild", "10.1.0.4", "child", "node");

    put_test_service_and_endpoint(
        &mut store,
        "storage-service",
        "10.1.0.1:8085:storage-service",
        "10.1.0.1",
    );
    put_test_service_and_endpoint(
        &mut store,
        "judge-worker",
        "10.1.0.2:8090:judge-worker",
        "10.1.0.2",
    );
    put_test_service_and_endpoint(
        &mut store,
        "sibling-storage",
        "10.1.0.3:8085:sibling-storage",
        "10.1.0.3",
    );
    put_test_service_and_endpoint(
        &mut store,
        "child-api",
        "10.1.0.2:8088:child-api",
        "10.1.0.2",
    );
    put_test_service_and_endpoint(
        &mut store,
        "grandchild-api",
        "10.1.0.4:8089:grandchild-api",
        "10.1.0.4",
    );

    put_test_api(
        &mut store,
        "storage-service",
        "storage.object.get",
        "/api/storage/objects",
        "descendants",
        "storage.object.read",
        "10.1.0.1:8085:storage-service",
        "running",
    );
    put_test_api(
        &mut store,
        "storage-service",
        "storage.object.put",
        "/api/storage/objects",
        "descendants",
        "storage.object.write",
        "10.1.0.1:8085:storage-service",
        "running",
    );
    put_test_api(
        &mut store,
        "storage-service",
        "storage.private.admin",
        "/api/storage/admin",
        "private",
        "storage.admin",
        "10.1.0.1:8085:storage-service",
        "running",
    );
    put_test_api(
        &mut store,
        "sibling-storage",
        "storage.sibling.get",
        "/api/sibling-storage/objects",
        "descendants",
        "storage.object.read",
        "10.1.0.3:8085:sibling-storage",
        "running",
    );
    put_test_api(
        &mut store,
        "child-api",
        "child.local",
        "/api/child",
        "same-node",
        "public",
        "10.1.0.2:8088:child-api",
        "running",
    );
    put_test_api(
        &mut store,
        "grandchild-api",
        "grandchild.local",
        "/api/grandchild",
        "same-node",
        "public",
        "10.1.0.4:8089:grandchild-api",
        "running",
    );

    let routes = store
        .effective_api_routes("child")
        .expect("child effective routes");
    let api_ids = routes
        .iter()
        .map(|route| route.api_id.as_str())
        .collect::<Vec<_>>();
    assert!(api_ids.contains(&"storage.object.get"));
    assert!(api_ids.contains(&"storage.object.put"));
    assert!(api_ids.contains(&"child.local"));
    assert!(!api_ids.contains(&"storage.sibling.get"));
    assert!(!api_ids.contains(&"storage.private.admin"));
    assert!(!api_ids.contains(&"grandchild.local"));
    assert_eq!(
        routes
            .iter()
            .find(|route| route.api_id == "storage.object.get")
            .map(|route| (
                route.provider_node_id.as_str(),
                route.provider_endpoint.as_str(),
                route.distance
            )),
        Some(("root", "10.1.0.1:8085:storage-service", 1))
    );
    assert!(
        store.links().is_empty(),
        "effective view does not require manual link"
    );

    store
        .upsert_deployed_service_api(DeployedServiceApi {
            host_ip: "10.1.0.1".to_string(),
            service_name: "storage-service".to_string(),
            version: "0.1.0".to_string(),
            endpoint: "10.1.0.1:8085:storage-service".to_string(),
            api_id: "storage.object.get".to_string(),
            status: "stopped".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("stop storage get api");
    let routes = store
        .effective_api_routes("child")
        .expect("child effective routes after stop");
    assert!(
        !routes
            .iter()
            .any(|route| route.api_id == "storage.object.get")
    );
}

fn local_http_endpoint(health_path: &str, response: &'static str) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local http listener");
    let socket_addr = listener.local_addr().expect("local addr").to_string();
    let endpoint = format!("{socket_addr}:gateway");
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

fn test_node(node_id: &str, host_ip: &str, parent_node_id: &str, role: &str) -> NodeRecord {
    NodeRecord {
        node_id: node_id.to_string(),
        host_ip: host_ip.to_string(),
        parent_node_id: parent_node_id.to_string(),
        role: role.to_string(),
        labels: serde_json::json!({}),
        status: "running".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn put_test_node(
    store: &mut MemoryOrchestratorStore,
    node_id: &str,
    host_ip: &str,
    parent_node_id: &str,
    role: &str,
) {
    store
        .upsert_node(test_node(node_id, host_ip, parent_node_id, role))
        .expect("put test node");
}

fn put_test_service_and_endpoint(
    store: &mut MemoryOrchestratorStore,
    service_id: &str,
    endpoint: &str,
    host_ip: &str,
) {
    let mut service = valid_service();
    service.id = service_id.to_string();
    service.name = service_id.to_string();
    service.endpoint.default_port = parse_endpoint_id(endpoint)
        .expect("endpoint identity")
        .port
        .parse()
        .expect("endpoint port");
    service.permissions = vec![
        "storage.object.read".to_string(),
        "storage.object.write".to_string(),
        "storage.admin".to_string(),
        "public".to_string(),
    ];
    store
        .put_service(service.clone())
        .expect("put test service");
    store
        .upsert_host_service(HostService {
            host_ip: host_ip.to_string(),
            service_name: service_id.to_string(),
            version: service.version.clone(),
            status: "running".to_string(),
            config: serde_json::json!({}),
            labels: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put host service");
    store
        .put_endpoint(Endpoint {
            endpoint: endpoint.to_string(),
            service_id: service_id.to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
            reachable: true,
            display_name: service_id.to_string(),
            note: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put endpoint");
}

fn put_test_api(
    store: &mut MemoryOrchestratorStore,
    service_name: &str,
    api_id: &str,
    path_prefix: &str,
    visibility: &str,
    permission: &str,
    endpoint: &str,
    status: &str,
) {
    store
        .upsert_service_api_surface(ServiceApiSurface {
            service_name: service_name.to_string(),
            version: "0.1.0".to_string(),
            api_id: api_id.to_string(),
            protocol: "http".to_string(),
            port_name: "http".to_string(),
            path_prefix: path_prefix.to_string(),
            methods: vec!["GET".to_string()],
            visibility: visibility.to_string(),
            auth_mode: if permission == "public" {
                "public".to_string()
            } else {
                "service".to_string()
            },
            permission: permission.to_string(),
            stability: "stable".to_string(),
            api_version: "v1".to_string(),
            rate_limit: String::new(),
            timeout: String::new(),
            config: serde_json::json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put api surface");
    let host_ip = parse_endpoint_id(endpoint)
        .expect("endpoint identity")
        .host
        .to_string();
    store
        .upsert_deployed_service_api(DeployedServiceApi {
            host_ip,
            service_name: service_name.to_string(),
            version: "0.1.0".to_string(),
            endpoint: endpoint.to_string(),
            api_id: api_id.to_string(),
            status: status.to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("put deployed api");
}

fn http_request_body_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let Ok(header) = std::str::from_utf8(&bytes[..header_end]) else {
        return false;
    };
    let header = header.to_ascii_lowercase();
    let content_length = header
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}
