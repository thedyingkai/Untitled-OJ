use crate::{ApiBinding, ServiceContext};
use anyhow::{Context as _, Result, anyhow};
use reqwest::{Client, Method, RequestBuilder};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, SystemTime},
};
use tokio::sync::watch;

const MAX_CONTEXT_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_CONTEXT_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    length: u64,
    modified: Option<SystemTime>,
    sha256: [u8; 32],
}

#[derive(Debug)]
struct ProviderSnapshot {
    context: Arc<ServiceContext>,
    identity: FileIdentity,
}

/// An accepted, immutable service-context generation.
#[derive(Debug, Clone)]
pub struct ContextUpdate {
    pub previous_generation: u64,
    pub current: Arc<ServiceContext>,
}

/// The normal typed result when an optional API binding is unresolved or was
/// removed by a newer Agent-published generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingUnavailable {
    pub name: String,
    pub generation: u64,
}

impl fmt::Display for BindingUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "API binding {:?} is unavailable at service context generation {}",
            self.name, self.generation
        )
    }
}

impl Error for BindingUnavailable {}

/// Concurrent, last-known-good view of an Agent-published context file.
///
/// Loading and validation happen before the write lock is acquired. Readers
/// therefore observe either the complete previous generation or the complete
/// next generation, never a partially decoded file. Invalid, missing, stale,
/// or same-generation-mutated files return an error and leave the last known
/// good snapshot untouched.
#[derive(Debug, Clone)]
pub struct ContextProvider {
    path: Arc<PathBuf>,
    state: Arc<RwLock<ProviderSnapshot>>,
    updates: watch::Sender<ContextUpdate>,
}

impl ContextProvider {
    /// Creates a provider only after synchronously loading a valid initial
    /// snapshot. This fail-closed boundary prevents callers from accidentally
    /// starting a managed workload without routing or credential metadata.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(anyhow!("service context provider path is required"));
        }
        let (context, identity) = read_candidate(&path)?;
        let context = Arc::new(context);
        let (updates, _) = watch::channel(ContextUpdate {
            previous_generation: 0,
            current: Arc::clone(&context),
        });
        Ok(Self {
            path: Arc::new(path),
            state: Arc::new(RwLock::new(ProviderSnapshot { context, identity })),
            updates,
        })
    }

    pub fn current(&self) -> Arc<ServiceContext> {
        Arc::clone(
            &self
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .context,
        )
    }

    /// Subscribes to accepted generations. The watch channel is bounded and
    /// coalescing, so a slow consumer observes the newest snapshot without
    /// blocking reload or accumulating unbounded memory.
    pub fn subscribe(&self) -> watch::Receiver<ContextUpdate> {
        self.updates.subscribe()
    }

    /// Loads and conditionally swaps one candidate. Returns `true` only when a
    /// strictly newer generation was accepted.
    pub fn reload_now(&self) -> Result<bool> {
        let (candidate, candidate_identity) = read_candidate(&self.path)?;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if candidate_identity == state.identity {
            return Ok(false);
        }
        let current_generation = state.context.generation;
        if candidate.generation < current_generation {
            return Err(anyhow!(
                "service context generation regression: current={current_generation} candidate={}",
                candidate.generation
            ));
        }
        if candidate.generation == current_generation {
            return Err(anyhow!(
                "service context generation {current_generation} was reused with different content"
            ));
        }
        let candidate = Arc::new(candidate);
        state.context = Arc::clone(&candidate);
        state.identity = candidate_identity;
        drop(state);
        self.updates.send_replace(ContextUpdate {
            previous_generation: current_generation,
            current: candidate,
        });
        Ok(true)
    }

    /// Polls until the supplied shutdown flag becomes true or its sender is
    /// dropped. Reload failures are intentionally non-fatal: callers retain the
    /// last-known-good snapshot and the next tick retries.
    pub async fn run(
        &self,
        poll_interval: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        if poll_interval.is_zero() {
            return Err(anyhow!("service context poll interval must be positive"));
        }
        let mut ticker = tokio::time::interval(poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the immediate first tick; the initial snapshot was already
        // loaded synchronously by `load`.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let _ = self.reload_now();
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }

    pub fn binding(&self, name: &str) -> std::result::Result<ApiBinding, BindingUnavailable> {
        let snapshot = self.current();
        snapshot
            .bindings
            .get(name)
            .cloned()
            .ok_or_else(|| BindingUnavailable {
                name: name.to_string(),
                generation: snapshot.generation,
            })
    }

    pub fn binding_url(&self, name: &str, relative_path: &str) -> Result<String> {
        let snapshot = self.current();
        if !snapshot.bindings.contains_key(name) {
            return Err(BindingUnavailable {
                name: name.to_string(),
                generation: snapshot.generation,
            }
            .into());
        }
        snapshot.binding_url(name, relative_path)
    }

    pub fn client(&self) -> Result<Client> {
        self.current().client()
    }

    /// Uses one coherent context generation for route, timeout and credential
    /// file selection. `ServiceContext::request` reads the token for every call,
    /// so credential rotation does not wait for a context generation change.
    pub async fn request(
        &self,
        client: &Client,
        binding_name: &str,
        method: Method,
        relative_path: &str,
    ) -> Result<RequestBuilder> {
        let snapshot = self.current();
        if !snapshot.bindings.contains_key(binding_name) {
            return Err(BindingUnavailable {
                name: binding_name.to_string(),
                generation: snapshot.generation,
            }
            .into());
        }
        snapshot
            .request(client, binding_name, method, relative_path)
            .await
    }
}

fn read_candidate(path: &Path) -> Result<(ServiceContext, FileIdentity)> {
    let mut file = File::open(path)
        .with_context(|| format!("open service context failed: {}", path.display()))?;
    let before = file
        .metadata()
        .with_context(|| format!("inspect service context failed: {}", path.display()))?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_CONTEXT_BYTES {
        return Err(anyhow!("service context must be a bounded regular file"));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.by_ref()
        .take(MAX_CONTEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read service context failed: {}", path.display()))?;
    if bytes.len() as u64 != before.len() || bytes.len() as u64 > MAX_CONTEXT_BYTES {
        return Err(anyhow!("service context changed while it was being loaded"));
    }
    let after = file
        .metadata()
        .with_context(|| format!("reinspect service context failed: {}", path.display()))?;
    if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
        return Err(anyhow!("service context changed while it was being loaded"));
    }
    let context: ServiceContext = serde_json::from_slice(&bytes)
        .with_context(|| format!("decode service context failed: {}", path.display()))?;
    context.validate()?;
    Ok((
        context,
        FileIdentity {
            length: bytes.len() as u64,
            modified: after.modified().ok(),
            sha256: Sha256::digest(&bytes).into(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, sync::Barrier, thread};
    use tempfile::TempDir;

    fn credential(root: &Path, value: &str) -> PathBuf {
        let path = root.join(format!("credential-{value}"));
        fs::write(&path, value).unwrap();
        path
    }

    fn context(root: &Path, generation: u64, provider: Option<&str>) -> ServiceContext {
        let mut bindings = BTreeMap::new();
        if let Some(provider) = provider {
            bindings.insert(
                "problems".to_string(),
                ApiBinding {
                    binding_id: provider.to_string(),
                    api_id: "problem.read".to_string(),
                    base_path: "/internal/apis/problem.read".to_string(),
                    timeout_ms: 1_000,
                },
            );
        }
        ServiceContext {
            schema_version: 1,
            deployment: crate::DeploymentIdentity {
                id: "deployment-1".to_string(),
                service: "contest-service".to_string(),
                node: "node-1".to_string(),
            },
            gateway: crate::GatewayContext {
                origin: "https://gateway.invalid".to_string(),
                ca_file: None,
            },
            bindings,
            credential_file: credential(root, &format!("token-{generation}")),
            generation,
        }
    }

    fn write(path: &Path, context: &ServiceContext) {
        let temporary = path.with_extension(format!("{}.tmp", context.generation));
        fs::write(&temporary, serde_json::to_vec(context).unwrap()).unwrap();
        fs::rename(temporary, path).unwrap();
    }

    #[test]
    fn optional_binding_add_remove_and_provider_switch_are_hot_reloaded() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("context.json");
        write(&path, &context(root.path(), 1, None));
        let provider = ContextProvider::load(&path).unwrap();
        let missing = provider.binding("problems").unwrap_err();
        assert_eq!(missing.generation, 1);

        write(&path, &context(root.path(), 2, Some("binding-a")));
        assert!(provider.reload_now().unwrap());
        assert_eq!(
            provider.binding("problems").unwrap().binding_id,
            "binding-a"
        );

        write(&path, &context(root.path(), 3, Some("binding-b")));
        provider.reload_now().unwrap();
        assert_eq!(
            provider.binding("problems").unwrap().binding_id,
            "binding-b"
        );

        write(&path, &context(root.path(), 4, None));
        provider.reload_now().unwrap();
        assert_eq!(provider.binding("problems").unwrap_err().generation, 4);
    }

    #[test]
    fn invalid_partial_regression_and_same_generation_mutation_keep_lkg() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("context.json");
        write(&path, &context(root.path(), 2, Some("binding-a")));
        let provider = ContextProvider::load(&path).unwrap();

        fs::write(&path, b"{\"generation\":3").unwrap();
        assert!(provider.reload_now().is_err());
        assert_eq!(provider.current().generation, 2);

        write(&path, &context(root.path(), 1, Some("binding-old")));
        assert!(provider.reload_now().is_err());
        assert_eq!(
            provider.binding("problems").unwrap().binding_id,
            "binding-a"
        );

        write(&path, &context(root.path(), 2, Some("binding-mutated")));
        assert!(provider.reload_now().is_err());
        assert_eq!(
            provider.binding("problems").unwrap().binding_id,
            "binding-a"
        );

        write(&path, &context(root.path(), 3, Some("binding-recovered")));
        assert!(provider.reload_now().unwrap());
        assert_eq!(
            provider.binding("problems").unwrap().binding_id,
            "binding-recovered"
        );
    }

    #[tokio::test]
    async fn subscribe_is_coalescing_and_watcher_stops() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("context.json");
        write(&path, &context(root.path(), 1, None));
        let provider = ContextProvider::load(&path).unwrap();
        let mut updates = provider.subscribe();
        let (stop_tx, stop_rx) = watch::channel(false);
        let watcher = {
            let provider = provider.clone();
            tokio::spawn(async move { provider.run(Duration::from_millis(5), stop_rx).await })
        };
        write(&path, &context(root.path(), 2, Some("binding-a")));
        tokio::time::timeout(Duration::from_secs(1), updates.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updates.borrow_and_update().current.generation, 2);
        stop_tx.send(true).unwrap();
        watcher.await.unwrap().unwrap();
    }

    #[test]
    fn concurrent_readers_observe_only_complete_generations() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("context.json");
        write(&path, &context(root.path(), 1, Some("binding-1")));
        let provider = ContextProvider::load(&path).unwrap();
        let barrier = Arc::new(Barrier::new(17));
        let mut readers = Vec::new();
        for _ in 0..16 {
            let provider = provider.clone();
            let barrier = Arc::clone(&barrier);
            readers.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..1_000 {
                    let snapshot = provider.current();
                    let binding = snapshot.bindings.get("problems").unwrap();
                    assert_eq!(
                        binding.binding_id,
                        format!("binding-{}", snapshot.generation)
                    );
                }
            }));
        }
        barrier.wait();
        for generation in 2..50 {
            write(
                &path,
                &context(
                    root.path(),
                    generation,
                    Some(&format!("binding-{generation}")),
                ),
            );
            provider.reload_now().unwrap();
        }
        for reader in readers {
            reader.join().unwrap();
        }
    }
}
