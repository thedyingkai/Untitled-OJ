use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_DEPENDENCIES: &[&str] = &[
    "postgres",
    "tokio-postgres",
    "r2d2_postgres",
    "rusqlite",
    "sqlx",
    "ureq",
    "reqwest",
    "flate2",
    "tar",
    "zip",
    "bollard",
];

const FORBIDDEN_SOURCE_PATTERNS: &[&str] = &[
    "std::fs",
    "std::io",
    "std::process",
    "std::net::TcpStream",
    "std::net::UdpSocket",
    "ToSocketAddrs",
    "Command::new",
    "TcpStream::connect",
    "ureq::",
    "postgres::",
    "rusqlite::",
    "sqlx::",
    "flate2::",
    "tar::",
    "zip::",
    "bollard::",
];

#[test]
fn core_manifest_has_no_infrastructure_dependencies() {
    let root = manifest_root();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read core Cargo.toml");
    let dependencies = manifest
        .lines()
        .skip_while(|line| line.trim() != "[dependencies]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .collect::<Vec<_>>();
    assert!(!dependencies.is_empty(), "core dependencies section");

    for dependency in FORBIDDEN_DEPENDENCIES {
        let declaration = format!("{dependency} =");
        assert!(
            !dependencies
                .iter()
                .any(|line| line.trim_start().starts_with(&declaration)),
            "pure orchestrator-core must not depend on {dependency}; put the adapter in orchestrator-legacy/runtime/storage"
        );
    }
    assert!(
        !manifest.contains("legacy-infrastructure"),
        "the core crate must not hide infrastructure behind an internal feature"
    );
}

#[test]
fn core_sources_have_no_direct_io_or_runtime_execution() {
    let src = manifest_root().join("src");
    let mut rust_files = Vec::new();
    collect_rust_files(&src, &mut rust_files);
    assert!(
        !rust_files.is_empty(),
        "core source files were not discovered"
    );

    for path in rust_files {
        let source = fs::read_to_string(&path).expect("read core source");
        for pattern in FORBIDDEN_SOURCE_PATTERNS {
            assert!(
                !source.contains(pattern),
                "{} contains forbidden infrastructure pattern {pattern}",
                path.display()
            );
        }
    }
}

#[test]
fn legacy_console_is_an_explicit_external_crate_boundary() {
    let core = manifest_root();
    let orchestrator_dir = core.parent().expect("orchestrator directory");
    let legacy = orchestrator_dir.join("legacy");
    let legacy_lib = fs::read_to_string(legacy.join("src/lib.rs")).expect("legacy crate lib");
    assert!(legacy_lib.contains("mod dispatcher;"));
    assert!(legacy_lib.contains("mod executor;"));
    assert!(legacy_lib.contains("mod store;"));

    let backend_manifest = fs::read_to_string(orchestrator_dir.join("backend/Cargo.toml"))
        .expect("backend Cargo.toml");
    assert!(
        backend_manifest.contains("orchestrator-legacy.workspace = true"),
        "backend compatibility console must enter through orchestrator-legacy"
    );
    assert!(
        !backend_manifest.contains("orchestrator-core"),
        "v1 backend must not depend on orchestrator-core through a hidden infrastructure feature or alias"
    );
}

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn collect_rust_files(dir: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read core src directory") {
        let entry = entry.expect("read core src entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}
