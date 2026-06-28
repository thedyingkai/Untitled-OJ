use crate::*;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn valid_service() -> ServiceManifest {
    ServiceManifest {
        schema_version: 1,
        id: "demo-api".to_string(),
        name: "Demo API".to_string(),
        version: "0.1.0".to_string(),
        kind: "backend-http".to_string(),
        endpoint: EndpointDecl {
            protocol: "http".to_string(),
            default_port: 18080,
            health_path: "/health".to_string(),
        },
        runtime: ServiceRuntimeDecl {
            mode: RuntimeMode::Container,
            root_allowed: true,
            non_root_allowed: false,
        },
        config_schema: serde_json::json!({}),
        requires: Default::default(),
        provides: Default::default(),
        ui: Default::default(),
        permissions: vec!["demo.read".to_string()],
        security: Default::default(),
    }
}

#[test]
fn checked_in_service_manifests_validate() {
    let root = repo_root();
    for path in [
        "services/gateway/service.yaml",
        "services/web-shell/service.yaml",
        "services/problem-api/service.yaml",
        "services/judge-api/service.yaml",
        "services/judge-worker/service.yaml",
        "services/storage/service.yaml",
        "services/postgres/service.yaml",
    ] {
        validate_service_manifest_file(&root, Path::new(path))
            .unwrap_or_else(|err| panic!("{path} should validate: {err}"));
    }
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
kind: backend-http
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
    let set = validate_service_set_file(&root, Path::new("sets/distributed-root.yaml"))
        .expect("distributed root set");
    let expanded = expand_set(&set);
    assert!(expanded.services.contains(&"gateway".to_string()));
    assert!(!expanded.default_links.is_empty());
}

#[test]
fn service_install_plan_uses_service_actions() {
    let manifest = valid_service();
    let plan = service_install_plan(&manifest, &[]);
    assert!(plan.can_apply);
    assert_eq!(plan.service_id, "demo-api");
    assert!(
        plan.actions
            .iter()
            .any(|item| item.action == "insert_service")
    );
}

#[test]
fn package_checksum_verify_and_path_rejection() {
    let dir = tempdir().unwrap();
    let service_dir = dir.path().join("demo");
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("service.yaml"),
        serde_yaml::to_string(&valid_service()).unwrap(),
    )
    .unwrap();
    fs::write(service_dir.join("README.md"), "demo").unwrap();
    let package = dir.path().join("demo.ojossvc");
    let result = package_service(&service_dir, &package).unwrap();
    assert!(result.valid);
    assert_eq!(result.service_id, "demo-api");
    let verified = verify_package(&package).unwrap();
    assert!(verified.files_checked >= 3);
    assert_eq!(verified.package.unwrap().format, "ojos-service");

    fs::write(service_dir.join(".env"), "SECRET=value").unwrap();
    assert!(package_service(&service_dir, &dir.path().join("bad.ojossvc")).is_err());
}

#[test]
fn package_entry_rejects_symlink() {
    let dir = tempdir().unwrap();
    let package = dir.path().join("symlink-entry.ojossvc");
    {
        let file = fs::File::create(&package).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o120777);
        use std::io::Write;
        zip.start_file("link", options).unwrap();
        zip.write_all(b"service.yaml").unwrap();
        zip.finish().unwrap();
    }
    assert!(verify_package(&package).is_err());
}

#[test]
fn checked_in_gateway_package_verifies() {
    let dir = tempdir().unwrap();
    let package = dir.path().join("gateway.ojossvc");
    let service_dir = repo_root().join("services/gateway");
    let result = package_service(&service_dir, &package).unwrap();
    assert!(result.valid);
    assert_eq!(result.service_id, "gateway");
    let verified = verify_package(&package).unwrap();
    assert!(verified.valid);
}

#[test]
fn package_rejects_banned_entries_and_checksum_mismatch() {
    for rel in [
        ".env",
        ".tmp/file",
        "node_modules/pkg/index.js",
        "frontend/dist/app.js",
    ] {
        let dir = tempdir().unwrap();
        let service_dir = dir.path().join("demo");
        fs::create_dir_all(service_dir.join(rel).parent().unwrap()).unwrap();
        fs::write(
            service_dir.join("service.yaml"),
            serde_yaml::to_string(&valid_service()).unwrap(),
        )
        .unwrap();
        fs::write(service_dir.join("README.md"), "demo").unwrap();
        fs::write(service_dir.join(rel), "bad").unwrap();
        assert!(
            package_service(&service_dir, &dir.path().join("bad.ojossvc")).is_err(),
            "{rel} should be rejected"
        );
    }

    let dir = tempdir().unwrap();
    let package = dir.path().join("checksum-mismatch.ojossvc");
    {
        let file = fs::File::create(&package).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        use std::io::Write;
        let manifest_text = serde_yaml::to_string(&valid_service()).unwrap();
        zip.start_file("service.yaml", options).unwrap();
        zip.write_all(manifest_text.as_bytes()).unwrap();
        zip.start_file("checksums.sha256", options).unwrap();
        zip.write_all(
            b"0000000000000000000000000000000000000000000000000000000000000000  service.yaml\n",
        )
        .unwrap();
        let metadata = "package:\n  format: ojos-service\n  version: 1\n  created_by: test\n";
        zip.start_file("package.yaml", options).unwrap();
        zip.write_all(metadata.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    assert!(verify_package(&package).is_err());
}

#[test]
fn package_requires_metadata() {
    let dir = tempdir().unwrap();
    let package = dir.path().join("missing-metadata.ojossvc");
    {
        let file = fs::File::create(&package).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        let manifest_text = serde_yaml::to_string(&valid_service()).unwrap();
        zip.start_file("service.yaml", options).unwrap();
        use std::io::Write;
        zip.write_all(manifest_text.as_bytes()).unwrap();
        let hash = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(manifest_text.as_bytes()))
        };
        zip.start_file("checksums.sha256", options).unwrap();
        zip.write_all(format!("{}  service.yaml\n", hash).as_bytes())
            .unwrap();
        zip.finish().unwrap();
    }
    assert!(verify_package(&package).is_err());
}
