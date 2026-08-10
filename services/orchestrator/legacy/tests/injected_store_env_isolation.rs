use orchestrator_legacy::{
    MemoryOrchestratorStore, OrchestratorActionConsole, OrchestratorStore,
    validate_service_manifest_file,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

struct EnvironmentRestore {
    name: &'static str,
    value: Option<OsString>,
}

impl EnvironmentRestore {
    fn set(name: &'static str, value: &str) -> Self {
        let restore = Self {
            name,
            value: std::env::var_os(name),
        };
        // SAFETY: This integration-test binary contains only this test, so no
        // other thread can concurrently read or mutate the process environment.
        unsafe { std::env::set_var(name, value) };
        restore
    }
}

impl Drop for EnvironmentRestore {
    fn drop(&mut self) {
        // SAFETY: See `EnvironmentRestore::set`; this test owns the environment
        // mutation for the lifetime of the guard.
        unsafe {
            match &self.value {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

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
        assert!(current.pop(), "repo root");
    }
}

fn assert_injected_store_loads(repo_root: &Path, database_url: &str) {
    let _environment = EnvironmentRestore::set("ORCHESTRATOR_DATABASE_URL", database_url);
    let mut injected_store = MemoryOrchestratorStore::new();
    let mut injected_only =
        validate_service_manifest_file(repo_root, Path::new("services/gateway/service.yaml"))
            .expect("fixture service manifest");
    injected_only.id = "injected-only".to_string();
    injected_only.name = "Injected-only service".to_string();
    injected_store
        .put_service(injected_only)
        .expect("seed injected store");

    let console =
        OrchestratorActionConsole::load_with_store(repo_root, "injected-memory", injected_store)
            .expect("injected store loading must ignore ORCHESTRATOR_DATABASE_URL");

    assert_eq!(console.store_kind(), "injected-memory");
    assert!(
        console
            .view()
            .expect("injected store view")
            .services
            .iter()
            .any(|service| service.id == "injected-only"),
        "the console view must come from the injected store"
    );
}

#[test]
fn injected_store_ignores_database_url_environment() {
    let root = repo_root();
    assert_injected_store_loads(&root, "this is not a PostgreSQL URL");
    assert_injected_store_loads(
        &root,
        "postgresql://orchestrator:secret@127.0.0.1:1/orchestrator?sslmode=require&connect_timeout=1",
    );
}
