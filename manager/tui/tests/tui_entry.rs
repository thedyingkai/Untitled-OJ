use std::path::PathBuf;
use std::process::Command;

#[test]
fn tui_help_is_available() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("repo root")
        .to_path_buf();
    let output = Command::new(env!("CARGO_BIN_EXE_ojos-orchestrator-tui"))
        .arg("--repo-root")
        .arg(repo_root)
        .arg("--help")
        .output()
        .expect("run tui help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    assert!(stdout.contains("OJOS Orchestrator"));
}
