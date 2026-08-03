use std::env;

const DEVELOPMENT_COMMIT: &str = "development";

fn canonical_commit(raw: Option<String>) -> String {
    raw.map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| DEVELOPMENT_COMMIT.to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=OJOS_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");

    // An explicit OJOS_BUILD_COMMIT wins even when malformed. Production then
    // fails closed instead of silently claiming the workflow commit.
    let raw_commit = env::var("OJOS_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env::var("GITHUB_SHA").ok());
    let commit = canonical_commit(raw_commit);
    let target = env::var("TARGET").expect("Cargo always supplies TARGET to build scripts");

    println!("cargo:rustc-env=OJOS_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=OJOS_BUILD_TARGET={target}");
}
