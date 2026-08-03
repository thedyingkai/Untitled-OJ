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
    assert!(stdout.contains("--api-url"));
    assert!(stdout.contains("--command"));
    assert!(stdout.contains("--oidc-issuer"));
    assert!(stdout.contains("--oidc-client-id"));
}

#[test]
fn remote_tui_requires_device_flow_configuration_and_never_reads_bearer_fallback() {
    let output = Command::new(env!("CARGO_BIN_EXE_ojos-orchestrator-tui"))
        .args([
            "--api-url",
            "https://control.example",
            "--command",
            "capabilities",
        ])
        .env_remove("OJOS_TUI_OIDC_ISSUER")
        .env_remove("OJOS_TUI_OIDC_CLIENT_ID")
        .env("OJOS_TUI_BEARER_TOKEN", "must-not-be-used")
        .output()
        .expect("run remote TUI preflight");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert!(stderr.contains("OIDC_ISSUER"));
    assert!(stderr.contains("bearer-token fallback is disabled"));
}

#[test]
fn v1_tui_does_not_silently_fall_back_to_the_legacy_local_console() {
    let output = Command::new(env!("CARGO_BIN_EXE_ojos-orchestrator-tui"))
        .env_remove("OJOS_ORCHESTRATOR_URL")
        .output()
        .expect("run TUI without remote endpoint");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert!(stderr.contains("requires --api-url"));
    assert!(stderr.contains("--legacy-local"));
}
