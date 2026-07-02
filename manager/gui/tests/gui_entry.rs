use std::path::PathBuf;
use std::process::Command;

#[test]
fn gui_entry_help_is_available_without_starting_window() {
    let output = Command::new(env!("CARGO_BIN_EXE_ojos-orchestrator-gui"))
        .arg("--help")
        .output()
        .expect("run gui help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    assert!(stdout.contains("OJOS Orchestrator"));
    assert!(stdout.contains("--repo-root"));
}

#[test]
fn gui_entry_uses_core_view_model() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("repo root")
        .to_path_buf();
    let view = orchestrator_core::load_orchestrator_view_with_database_url(&repo_root, None)
        .expect("GUI should load core view");
    assert!(!view.services.is_empty());
    assert!(!view.templates.is_empty());
    assert!(!view.endpoints.is_empty());
    assert!(!view.links.is_empty());
    assert!(!view.operations.is_empty());
    assert!(!view.logs.is_empty());
    assert!(!view.diagnostics.is_empty());
}
