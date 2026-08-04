use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Parser, Subcommand};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use walkdir::WalkDir;

const PRODUCT: &str = "ojos-orchestrator";
const BUNDLE_MANIFEST: &str = ".ojos-bundle.json";
const INSTALL_MARKER: &str = ".ojos-orchestrator-install.json";
const BUNDLE_SCHEMA_VERSION: u32 = 1;

/// Shared lifetime lock held by installed OJOS processes. The native
/// installer takes the corresponding exclusive lock before replacing files,
/// preventing old binaries from running against a new resource tree.
#[derive(Debug)]
pub struct RuntimeInstallGuard {
    _lock: Option<File>,
}

pub fn acquire_runtime_install_guard() -> Result<RuntimeInstallGuard> {
    let executable =
        std::env::current_exe().context("resolve current executable for install lock")?;
    acquire_runtime_install_guard_from(&executable)
}

fn acquire_runtime_install_guard_from(executable: &Path) -> Result<RuntimeInstallGuard> {
    let Some(root) = executable
        .parent()
        .filter(|parent| parent.file_name() == Some(OsStr::new("bin")))
        .and_then(Path::parent)
    else {
        return Ok(RuntimeInstallGuard { _lock: None });
    };
    let marker_path = root.join(INSTALL_MARKER);
    if !marker_path.is_file() {
        return Ok(RuntimeInstallGuard { _lock: None });
    }
    let marker: InstallMarker = read_json(&marker_path)?;
    ensure!(
        marker.product == PRODUCT,
        "installed runtime marker belongs to another product"
    );
    let install_name = root
        .file_name()
        .ok_or_else(|| anyhow!("installed runtime root has no directory name"))?;
    let parent = root
        .parent()
        .ok_or_else(|| anyhow!("installed runtime root has no parent directory"))?;
    let lock_path = parent.join(format!(".{}.install.lock", install_name.to_string_lossy()));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open runtime install lock {}", lock_path.display()))?;
    FileExt::try_lock_shared(&lock).map_err(|error| {
        anyhow!(
            "OJOS installation or upgrade is in progress for {}: {error}",
            root.display()
        )
    })?;
    Ok(RuntimeInstallGuard { _lock: Some(lock) })
}

#[derive(Debug, Parser)]
#[command(name = "ojos-orchestrator")]
#[command(about = "Native installer and launcher for OJOS Orchestrator")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Install or atomically upgrade an extracted OJOS bundle.
    Install {
        /// Payload directory. Defaults to the payload directory beside this executable.
        #[arg(long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Installation directory. Defaults to the current user's application directory.
        #[arg(long, value_name = "DIR")]
        prefix: Option<PathBuf>,
        /// Do not add the installed bin directory to the Windows user PATH.
        #[arg(long)]
        no_path: bool,
    },
    /// Verify every file and runtime reference in an extracted payload.
    Verify {
        #[arg(long, value_name = "DIR")]
        bundle: Option<PathBuf>,
    },
    /// Build a verified bundle from already-built repository artifacts.
    #[command(hide = true)]
    Pack {
        #[arg(long, value_name = "DIR", default_value = ".")]
        repo_root: PathBuf,
        #[arg(long, value_name = "DIR")]
        target_dir: Option<PathBuf>,
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
        #[arg(long, value_name = "SHA")]
        commit: Option<String>,
    },
    /// Verify and report the current user installation.
    Status {
        #[arg(long, value_name = "DIR")]
        prefix: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Launch the installed embedded Desktop application.
    Start {
        #[arg(long, value_name = "DIR")]
        prefix: Option<PathBuf>,
        /// Wait for Desktop to exit and return its exit status.
        #[arg(long)]
        wait: bool,
        #[arg(last = true)]
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BundleManifest {
    schema_version: u32,
    product: String,
    version: String,
    target_os: String,
    target_arch: String,
    source_commit: String,
    files: Vec<BundleFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BundleFile {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallMarker {
    schema_version: u32,
    product: String,
    version: String,
    source_commit: String,
    bundle_sha256: String,
    installed_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum JournalPhase {
    Prepared,
    OldMoved,
    NewPublished,
}

impl JournalPhase {
    fn file_part(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::OldMoved => "old-moved",
            Self::NewPublished => "new-published",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallJournal {
    schema_version: u32,
    product: String,
    nonce: String,
    phase: JournalPhase,
    install_root: PathBuf,
    stage: PathBuf,
    backup: Option<PathBuf>,
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Commands::Install {
            bundle,
            prefix,
            no_path,
        }) => install(bundle.as_deref(), prefix.as_deref(), no_path),
        Some(Commands::Verify { bundle }) => {
            let root = resolve_bundle_root(bundle.as_deref())?;
            let manifest = verify_bundle(&root)?;
            println!(
                "verified OJOS Orchestrator {} payload for {}-{} ({} files)",
                manifest.version,
                manifest.target_os,
                manifest.target_arch,
                manifest.files.len()
            );
            Ok(())
        }
        Some(Commands::Pack {
            repo_root,
            target_dir,
            output,
            commit,
        }) => pack(
            &repo_root,
            target_dir.as_deref(),
            &output,
            commit.as_deref(),
        ),
        Some(Commands::Status { prefix, json }) => status(prefix.as_deref(), json),
        Some(Commands::Start { prefix, wait, args }) => start(prefix.as_deref(), wait, &args),
        None => start(None, false, &[]),
    }
}

fn executable_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn default_install_root() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("LOCALAPPDATA is not set"))?;
        return Ok(base.join("Programs").join("OJOS-Orchestrator"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(base) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(base).join("ojos-orchestrator"));
        }
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        return Ok(home.join(".local/share/ojos-orchestrator"));
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    bail!("OJOS native installation currently supports Windows and Linux")
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(normalized.pop(), "path escapes its filesystem root");
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn resolve_bundle_root(explicit: Option<&Path>) -> Result<PathBuf> {
    let candidate = match explicit {
        Some(path) => absolute_path(path)?,
        None => std::env::current_exe()
            .context("resolve installer executable")?
            .parent()
            .ok_or_else(|| anyhow!("installer executable has no parent directory"))?
            .join("payload"),
    };
    fs::canonicalize(&candidate)
        .with_context(|| format!("bundle payload {} does not exist", candidate.display()))
}

fn install(bundle: Option<&Path>, prefix: Option<&Path>, no_path: bool) -> Result<()> {
    let bundle_root = resolve_bundle_root(bundle)?;
    let manifest = verify_bundle(&bundle_root)?;
    let requested_root = absolute_path(
        &prefix
            .map(Path::to_path_buf)
            .unwrap_or(default_install_root()?),
    )?;
    let install_name = requested_root
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("install prefix must be a dedicated directory"))?
        .to_owned();
    let requested_parent = requested_root
        .parent()
        .ok_or_else(|| anyhow!("install prefix cannot be a filesystem root"))?;
    fs::create_dir_all(requested_parent)
        .with_context(|| format!("create install parent {}", requested_parent.display()))?;
    let parent = canonical_path(requested_parent)
        .with_context(|| format!("resolve install parent {}", requested_parent.display()))?;
    let install_root = parent.join(&install_name);
    ensure!(
        !path_eq(&bundle_root, &install_root),
        "bundle payload and install prefix must be different directories"
    );

    let lock_path = parent.join(format!(".{}.install.lock", install_name.to_string_lossy()));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open installer lock {}", lock_path.display()))?;
    lock.try_lock_exclusive().map_err(|error| {
        anyhow!(
            "another install/upgrade is running or an installed OJOS process is still active for {}; close Desktop, daemon, TUI, and Agent before upgrading: {error}",
            install_root.display()
        )
    })?;

    recover_interrupted_install(&parent, &install_name, &install_root)?;
    validate_existing_install(&install_root)?;

    let nonce = unique_nonce();
    let stage = parent.join(format!(
        ".{}.installing.{nonce}",
        install_name.to_string_lossy()
    ));
    let backup = parent.join(format!(
        ".{}.previous.{nonce}",
        install_name.to_string_lossy()
    ));
    fs::create_dir(&stage).with_context(|| format!("create stage {}", stage.display()))?;

    let result = (|| -> Result<()> {
        copy_manifest_files(&bundle_root, &stage, &manifest)?;
        copy_file_synced(
            &bundle_root.join(BUNDLE_MANIFEST),
            &stage.join(BUNDLE_MANIFEST),
        )?;
        let marker = InstallMarker {
            schema_version: 1,
            product: PRODUCT.to_string(),
            version: manifest.version.clone(),
            source_commit: manifest.source_commit.clone(),
            bundle_sha256: sha256_file(&bundle_root.join(BUNDLE_MANIFEST))?,
            installed_at: unix_timestamp()?.to_string(),
        };
        write_json_synced(&stage.join(INSTALL_MARKER), &marker)?;
        verify_installed_tree(&stage)?;
        sync_directory(&stage)?;

        let mut journal = InstallJournal {
            schema_version: 1,
            product: PRODUCT.to_string(),
            nonce: nonce.clone(),
            phase: JournalPhase::Prepared,
            install_root: install_root.clone(),
            stage: stage.clone(),
            backup: install_root.exists().then(|| backup.clone()),
        };
        let mut journal_files = Vec::new();
        journal_files.push(write_journal(&parent, &install_name, &journal)?);

        if install_root.exists() {
            fs::rename(&install_root, &backup).with_context(|| {
                format!(
                    "replace {} (close running OJOS processes before upgrading)",
                    install_root.display()
                )
            })?;
        }
        journal.phase = JournalPhase::OldMoved;
        journal_files.push(write_journal(&parent, &install_name, &journal)?);
        remove_older_journals(&journal_files)?;

        if let Err(error) = fs::rename(&stage, &install_root) {
            if backup.exists() && !install_root.exists() {
                fs::rename(&backup, &install_root)
                    .context("restore previous installation after publish failure")?;
            }
            bail!("publish new installation: {error}");
        }
        sync_directory(&parent)?;
        verify_installed_tree(&install_root)?;
        journal.phase = JournalPhase::NewPublished;
        journal_files.push(write_journal(&parent, &install_name, &journal)?);
        remove_older_journals(&journal_files)?;

        if backup.exists() {
            if let Err(error) = remove_verified_install_tree(&backup, &parent) {
                eprintln!(
                    "warning: the new installation is active, but the verified backup {} could not be removed: {error}",
                    backup.display()
                );
            }
        }
        remove_all_journals(&parent, &install_name, Some(&nonce))?;
        sync_directory(&parent)?;
        Ok(())
    })();

    if let Err(error) = result {
        let recovery = recover_interrupted_install(&parent, &install_name, &install_root);
        if stage.exists() {
            let _ = remove_verified_stage(&stage, &parent);
        }
        if let Err(recovery_error) = recovery {
            bail!("installation failed: {error}; automatic recovery also failed: {recovery_error}");
        }
        return Err(error).context("installation failed; the previous installation was restored");
    }

    let installed_bin = install_root.join("bin");
    #[cfg(windows)]
    if !no_path {
        if let Err(error) = windows_path::add_to_user_path(&installed_bin) {
            eprintln!(
                "warning: OJOS is installed, but the optional Windows user PATH update failed: {error}"
            );
        }
    }
    #[cfg(not(windows))]
    let _ = no_path;

    println!(
        "OJOS Orchestrator {} installed at {}",
        manifest.version,
        install_root.display()
    );
    println!(
        "Start: {} start",
        installed_bin
            .join(executable_name("ojos-orchestrator"))
            .display()
    );
    #[cfg(target_os = "linux")]
    println!(
        "To call it as 'ojos-orchestrator', add {} to your shell PATH.",
        installed_bin.display()
    );
    Ok(())
}

fn status(prefix: Option<&Path>, json: bool) -> Result<()> {
    let root = resolve_installed_root(prefix)?;
    let manifest = verify_installed_tree(&root)?;
    let marker: InstallMarker = read_json(&root.join(INSTALL_MARKER))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "installed": true,
                "path": root,
                "version": manifest.version,
                "source_commit": marker.source_commit,
                "target_os": manifest.target_os,
                "target_arch": manifest.target_arch,
                "files": manifest.files.len()
            }))?
        );
    } else {
        println!(
            "OJOS Orchestrator {} is installed at {} (commit {})",
            manifest.version,
            root.display(),
            marker.source_commit
        );
    }
    Ok(())
}

fn start(prefix: Option<&Path>, wait: bool, args: &[String]) -> Result<()> {
    let root = resolve_installed_root(prefix)?;
    let _runtime_guard = acquire_runtime_install_guard_from(
        &root.join("bin").join(executable_name("ojos-orchestrator")),
    )?;
    verify_installed_tree(&root)?;
    let desktop = root
        .join("bin")
        .join(executable_name("ojos-orchestrator-desktop"));
    let ready_token = unique_nonce();
    let ready_path = std::env::temp_dir().join(format!(".ojos-desktop-ready-{ready_token}"));
    ensure!(
        !ready_path.exists(),
        "Desktop readiness path already exists: {}",
        ready_path.display()
    );
    let mut child = Command::new(&desktop)
        .args(args)
        .env("OJOS_DESKTOP_READY_FILE", &ready_path)
        .env("OJOS_DESKTOP_READY_TOKEN", &ready_token)
        .spawn()
        .with_context(|| format!("start Desktop {}", desktop.display()))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if ready_path.is_file() {
            let observed = fs::read_to_string(&ready_path)
                .context("read Desktop startup readiness acknowledgement")?;
            let _ = fs::remove_file(&ready_path);
            ensure!(
                observed.trim() == ready_token,
                "Desktop startup readiness acknowledgement did not match this launch"
            );
            break;
        }
        if let Some(status) = child.try_wait().context("query Desktop startup status")? {
            let _ = fs::remove_file(&ready_path);
            bail!("Desktop exited before its WebView became ready ({status})");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&ready_path);
            bail!("Desktop did not acknowledge WebView readiness within 30 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if wait {
        let status = child.wait().context("wait for Desktop")?;
        ensure!(status.success(), "Desktop exited with {status}");
    } else {
        println!("started OJOS Orchestrator Desktop (pid {})", child.id());
    }
    Ok(())
}

fn resolve_installed_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return absolute_path(path);
    }
    if let Some(candidate) = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf))
        .and_then(|bin| bin.parent().map(Path::to_path_buf))
        .filter(|root| root.join(INSTALL_MARKER).is_file() && root.join(BUNDLE_MANIFEST).is_file())
    {
        return canonical_path(&candidate);
    }
    absolute_path(&default_install_root()?)
}

fn pack(
    repo_root: &Path,
    target_dir: Option<&Path>,
    output: &Path,
    commit: Option<&str>,
) -> Result<()> {
    ensure!(
        cfg!(windows) || cfg!(target_os = "linux"),
        "pack supports Windows and Linux"
    );
    let repo_root = fs::canonicalize(repo_root)
        .with_context(|| format!("resolve repository root {}", repo_root.display()))?;
    ensure!(
        repo_root.join("Cargo.toml").is_file(),
        "repository Cargo.toml is missing"
    );
    let output = absolute_path(output)?;
    if output.exists() {
        ensure!(
            output.is_dir() && fs::read_dir(&output)?.next().is_none(),
            "pack output must be absent or empty: {}",
            output.display()
        );
    } else {
        fs::create_dir_all(&output)?;
    }
    let payload = output.join("payload");
    fs::create_dir(&payload)?;

    let target_dir = match target_dir {
        Some(path) => absolute_from(&repo_root, path)?,
        None => match std::env::var_os("CARGO_TARGET_DIR") {
            Some(path) => absolute_from(&repo_root, Path::new(&path))?.join("release"),
            None => repo_root.join("target/release"),
        },
    };
    let installer = std::env::current_exe().context("resolve pack executable")?;
    validate_native_binary(&installer)?;
    copy_file_synced(
        &installer,
        &output.join(executable_name("ojos-orchestrator")),
    )?;

    let bin_dir = payload.join("bin");
    fs::create_dir_all(&bin_dir)?;
    copy_file_synced(
        &installer,
        &bin_dir.join(executable_name("ojos-orchestrator")),
    )?;
    for name in [
        "ojos-orchestrator-daemon",
        "ojos-orchestrator-tui",
        "ojos-orchestrator-agent",
        "ojos-orchestrator-desktop",
    ] {
        let source = target_dir.join(executable_name(name));
        validate_native_binary(&source).with_context(|| format!("validate built binary {name}"))?;
        copy_file_synced(&source, &bin_dir.join(executable_name(name)))?;
    }
    #[cfg(windows)]
    copy_file_synced(
        &target_dir.join("WebView2Loader.dll"),
        &bin_dir.join("WebView2Loader.dll"),
    )?;

    let include_roots = discover_runtime_roots(&repo_root)?;
    for relative in git_tracked_files(&repo_root, &include_roots)? {
        copy_repo_file(&repo_root, &payload, &relative)?;
    }
    copy_generated_tree(
        &repo_root.join("manager/web/dist"),
        &payload.join("manager/web/dist"),
    )?;

    let source_commit = resolve_commit(&repo_root, commit)?;
    let manifest = build_manifest(&payload, &source_commit)?;
    write_json_synced(&payload.join(BUNDLE_MANIFEST), &manifest)?;
    verify_bundle(&payload)?;

    let install_command = if cfg!(windows) {
        format!(r".\{} install", executable_name("ojos-orchestrator"))
    } else {
        format!("./{} install", executable_name("ojos-orchestrator"))
    };
    let instructions = format!(
        "OJOS Orchestrator {}\n\nInstall from this extracted directory:\n  {install_command}\n\nNo shell, batch, or PowerShell installer is required.\n",
        env!("CARGO_PKG_VERSION"),
    );
    write_bytes_synced(&output.join("INSTALL.txt"), instructions.as_bytes())?;
    sync_directory(&output)?;
    println!("packed verified bundle at {}", output.display());
    Ok(())
}

fn discover_runtime_roots(repo_root: &Path) -> Result<Vec<String>> {
    let mut roots = BTreeSet::from([
        "platform/schemas/orchestrator".to_string(),
        "platform/shared/go".to_string(),
        "sets".to_string(),
        "store/index.json".to_string(),
        "docs/orchestrator/operations-v1.md".to_string(),
    ]);
    let services = repo_root.join("services");
    for entry in fs::read_dir(&services).context("read services directory")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let service = entry.file_name().to_string_lossy().into_owned();
        let release_path = entry.path().join("release.yaml");
        let service_path = entry.path().join("service.yaml");
        if service_path.is_file() {
            roots.insert(format!("services/{service}/service.yaml"));
            let service_manifest: YamlValue = serde_yaml::from_slice(&fs::read(&service_path)?)
                .with_context(|| format!("parse {}", service_path.display()))?;
            if yaml_string(&service_manifest, &["source", "type"]) == Some("local") {
                let source =
                    yaml_string(&service_manifest, &["source", "ref"]).ok_or_else(|| {
                        anyhow!("{} local source is missing ref", service_path.display())
                    })?;
                validate_manifest_path(source)?;
                roots.insert(source.to_string());
            }
            if let Some(build_path) = yaml_string(&service_manifest, &["source", "build", "path"])
                .filter(|value| !value.is_empty())
            {
                validate_manifest_path(build_path)?;
                roots.insert(build_path.to_string());
            }
        }
        if !release_path.is_file() {
            continue;
        }
        roots.insert(format!("services/{service}/release.yaml"));
        let release: YamlValue = serde_yaml::from_slice(&fs::read(&release_path)?)
            .with_context(|| format!("parse {}", release_path.display()))?;
        if yaml_string(&release, &["source", "kind"]) == Some("local") {
            let url = yaml_string(&release, &["source", "url"])
                .ok_or_else(|| anyhow!("{} local source is missing url", release_path.display()))?;
            let relative = url.strip_prefix("local://").ok_or_else(|| {
                anyhow!("{} has invalid local source {url}", release_path.display())
            })?;
            validate_manifest_path(relative)?;
            roots.insert(relative.to_string());
        }
        if let Some(working_dir) =
            yaml_string(&release, &["runtime", "working_dir"]).filter(|value| !value.is_empty())
        {
            validate_manifest_path(working_dir)?;
            roots.insert(working_dir.to_string());
        }
        if let Some(binary) =
            yaml_string(&release, &["runtime", "binary"]).filter(|value| !value.is_empty())
        {
            validate_manifest_path(binary)?;
            roots.insert(binary.to_string());
        }
        if let Some(migrations) = release.get("migrations").and_then(YamlValue::as_sequence) {
            for migration in migrations {
                let path = migration
                    .get("path")
                    .and_then(YamlValue::as_str)
                    .ok_or_else(|| {
                        anyhow!("{} migration is missing path", release_path.display())
                    })?;
                validate_manifest_path(path)?;
                roots.insert(path.to_string());
            }
        }
    }
    Ok(roots.into_iter().collect())
}

fn git_tracked_files(repo_root: &Path, roots: &[String]) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-z", "--"])
        .args(roots)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("run git ls-files for portable payload")?;
    ensure!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let mut files = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
    {
        let path = std::str::from_utf8(raw).context("tracked path is not UTF-8")?;
        validate_manifest_path(path)?;
        files.push(PathBuf::from(path));
    }
    files.sort();
    files.dedup();
    ensure!(!files.is_empty(), "git did not return any runtime files");
    Ok(files)
}

fn copy_repo_file(repo_root: &Path, payload: &Path, relative: &Path) -> Result<()> {
    let source = repo_root.join(relative);
    let metadata = fs::symlink_metadata(&source)
        .with_context(|| format!("read tracked runtime file {}", source.display()))?;
    ensure!(
        metadata.is_file(),
        "tracked runtime path is not a file: {}",
        source.display()
    );
    copy_file_synced(&source, &payload.join(relative))
}

fn copy_generated_tree(source: &Path, destination: &Path) -> Result<()> {
    ensure!(
        source.join("index.html").is_file(),
        "Web build is missing at {}",
        source.display()
    );
    for entry in WalkDir::new(source).sort_by_file_name() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_symlink() {
            bail!(
                "generated Web tree contains a symlink: {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            copy_file_synced(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn build_manifest(payload: &Path, source_commit: &str) -> Result<BundleManifest> {
    let mut files = Vec::new();
    for entry in WalkDir::new(payload).sort_by_file_name() {
        let entry = entry?;
        if entry.path() == payload || entry.file_type().is_dir() {
            continue;
        }
        ensure!(
            !entry.file_type().is_symlink(),
            "bundle contains a symlink: {}",
            entry.path().display()
        );
        ensure!(
            entry.file_type().is_file(),
            "bundle contains a non-file entry: {}",
            entry.path().display()
        );
        let relative = entry.path().strip_prefix(payload)?;
        let portable = portable_path(relative)?;
        if portable == BUNDLE_MANIFEST {
            continue;
        }
        files.push(BundleFile {
            path: portable,
            size: entry.metadata()?.len(),
            sha256: sha256_file(entry.path())?,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    ensure!(!files.is_empty(), "bundle payload is empty");
    Ok(BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        product: PRODUCT.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        target_os: std::env::consts::OS.to_string(),
        target_arch: std::env::consts::ARCH.to_string(),
        source_commit: source_commit.to_string(),
        files,
    })
}

fn verify_bundle(root: &Path) -> Result<BundleManifest> {
    let manifest_path = root.join(BUNDLE_MANIFEST);
    let manifest: BundleManifest = read_json(&manifest_path)?;
    ensure!(
        manifest.schema_version == BUNDLE_SCHEMA_VERSION,
        "unsupported bundle schema"
    );
    ensure!(
        manifest.product == PRODUCT,
        "bundle product is not OJOS Orchestrator"
    );
    ensure!(
        manifest.target_os == std::env::consts::OS,
        "bundle targets {}, current OS is {}",
        manifest.target_os,
        std::env::consts::OS
    );
    ensure!(
        manifest.target_arch == std::env::consts::ARCH,
        "bundle targets {}, current architecture is {}",
        manifest.target_arch,
        std::env::consts::ARCH
    );
    verify_manifest_files(root, &manifest, false)?;
    validate_required_layout(root)?;
    validate_runtime_references(root)?;
    validate_linux_runtime(root)?;
    Ok(manifest)
}

fn verify_installed_tree(root: &Path) -> Result<BundleManifest> {
    ensure!(root.is_dir(), "OJOS is not installed at {}", root.display());
    let marker: InstallMarker = read_json(&root.join(INSTALL_MARKER))?;
    ensure!(
        marker.product == PRODUCT,
        "install marker belongs to another product"
    );
    let manifest_path = root.join(BUNDLE_MANIFEST);
    ensure!(
        marker.bundle_sha256 == sha256_file(&manifest_path)?,
        "installed bundle manifest digest does not match the install marker"
    );
    let manifest: BundleManifest = read_json(&manifest_path)?;
    ensure!(
        manifest.product == PRODUCT,
        "installed bundle belongs to another product"
    );
    ensure!(
        manifest.version == marker.version,
        "installed marker and bundle versions differ"
    );
    verify_manifest_files(root, &manifest, true)?;
    validate_required_layout(root)?;
    validate_runtime_references(root)?;
    Ok(manifest)
}

fn verify_manifest_files(root: &Path, manifest: &BundleManifest, installed: bool) -> Result<()> {
    let mut declared = BTreeSet::new();
    for file in &manifest.files {
        validate_manifest_path(&file.path)?;
        ensure!(
            declared.insert(file.path.clone()),
            "duplicate bundle path {}",
            file.path
        );
        let path = safe_join(root, &file.path)?;
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("bundle file {} is missing", file.path))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "bundle path {} is not a regular file",
            file.path
        );
        ensure!(
            metadata.len() == file.size,
            "bundle file {} has the wrong size",
            file.path
        );
        ensure!(
            sha256_file(&path)? == file.sha256,
            "bundle file {} failed SHA-256 verification",
            file.path
        );
    }
    let mut actual = BTreeSet::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        if entry.path() == root || entry.file_type().is_dir() {
            continue;
        }
        ensure!(
            entry.file_type().is_file() && !entry.file_type().is_symlink(),
            "bundle has an unsupported entry: {}",
            entry.path().display()
        );
        let relative = portable_path(entry.path().strip_prefix(root)?)?;
        if relative == BUNDLE_MANIFEST || (installed && relative == INSTALL_MARKER) {
            continue;
        }
        actual.insert(relative);
    }
    ensure!(
        declared == actual,
        "bundle contains missing or undeclared files"
    );
    Ok(())
}

fn validate_required_layout(root: &Path) -> Result<()> {
    for name in [
        "ojos-orchestrator",
        "ojos-orchestrator-daemon",
        "ojos-orchestrator-tui",
        "ojos-orchestrator-agent",
        "ojos-orchestrator-desktop",
    ] {
        validate_native_binary(&root.join("bin").join(executable_name(name)))?;
    }
    for relative in [
        "manager/web/dist/index.html",
        "platform/schemas/orchestrator/actions-v1.yaml",
        "store/index.json",
    ] {
        ensure!(
            safe_join(root, relative)?.is_file(),
            "required bundle resource {relative} is missing"
        );
    }
    #[cfg(windows)]
    ensure!(
        root.join("bin/WebView2Loader.dll").is_file(),
        "WebView2Loader.dll is missing"
    );
    let service_count = fs::read_dir(root.join("services"))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().join("service.yaml").is_file()
                && entry.path().join("release.yaml").is_file()
        })
        .count();
    ensure!(
        service_count > 0,
        "bundle has no complete service manifests"
    );
    Ok(())
}

fn validate_runtime_references(root: &Path) -> Result<()> {
    let services = root.join("services");
    for entry in fs::read_dir(&services).context("read bundled services")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let service_path = entry.path().join("service.yaml");
        let release_path = entry.path().join("release.yaml");
        if !service_path.exists() && !release_path.exists() {
            continue;
        }
        ensure!(
            service_path.is_file() && release_path.is_file(),
            "bundled service {} must contain both service.yaml and release.yaml",
            entry.file_name().to_string_lossy()
        );
        let service_manifest: YamlValue = serde_yaml::from_slice(&fs::read(&service_path)?)
            .with_context(|| format!("parse bundled {}", service_path.display()))?;
        if yaml_string(&service_manifest, &["source", "type"]) == Some("local") {
            let source = yaml_string(&service_manifest, &["source", "ref"])
                .ok_or_else(|| anyhow!("{} local source is missing ref", service_path.display()))?;
            ensure!(
                safe_join(root, source)?.is_dir(),
                "bundled Service source {source} is missing"
            );
        }
        if let Some(build_path) = yaml_string(&service_manifest, &["source", "build", "path"])
            .filter(|value| !value.is_empty())
        {
            ensure!(
                safe_join(root, build_path)?.is_file(),
                "bundled Service build path {build_path} is missing"
            );
        }
        let release: YamlValue = serde_yaml::from_slice(&fs::read(&release_path)?)
            .with_context(|| format!("parse bundled {}", release_path.display()))?;
        if yaml_string(&release, &["source", "kind"]) == Some("local") {
            let url = yaml_string(&release, &["source", "url"])
                .ok_or_else(|| anyhow!("{} local source is missing url", release_path.display()))?;
            let path = url
                .strip_prefix("local://")
                .ok_or_else(|| anyhow!("invalid local source {url}"))?;
            ensure!(
                safe_join(root, path)?.is_dir(),
                "bundled local source {path} is missing"
            );
        }
        for keys in [["runtime", "working_dir"], ["runtime", "binary"]] {
            if let Some(path) = yaml_string(&release, &keys).filter(|value| !value.is_empty()) {
                ensure!(
                    safe_join(root, path)?.exists(),
                    "bundled runtime reference {path} is missing"
                );
            }
        }
        if let Some(migrations) = release.get("migrations").and_then(YamlValue::as_sequence) {
            for migration in migrations {
                let path = migration
                    .get("path")
                    .and_then(YamlValue::as_str)
                    .ok_or_else(|| {
                        anyhow!("{} migration is missing path", release_path.display())
                    })?;
                ensure!(
                    safe_join(root, path)?.is_file(),
                    "bundled migration {path} is missing"
                );
            }
        }
    }
    let index: JsonValue = read_json(&root.join("store/index.json"))?;
    let modules = index
        .get("modules")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| anyhow!("Store index modules are missing"))?;
    for module in modules {
        if let Some(source) = module
            .get("source_url")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
        {
            let source_root = safe_join(root, source)?;
            ensure!(
                source_root.is_dir(),
                "Store source {source} is missing from the bundle"
            );
            ensure!(
                source_root.join("service.yaml").is_file()
                    && source_root.join("release.yaml").is_file(),
                "Store source {source} is missing service.yaml or release.yaml"
            );
        }
    }
    Ok(())
}

fn validate_linux_runtime(root: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let desktop = root.join("bin/ojos-orchestrator-desktop");
        let output = Command::new("ldd")
            .arg(&desktop)
            .output()
            .context("run ldd for the Desktop runtime preflight")?;
        ensure!(
            output.status.success(),
            "ldd failed for {}",
            desktop.display()
        );
        let report = String::from_utf8_lossy(&output.stdout);
        ensure!(
            !report.contains("not found"),
            "Linux Desktop runtime libraries are missing. Install WebKitGTK 4.1, GTK3, Ayatana AppIndicator, librsvg, and libxdo for your distribution. ldd reported:\n{report}"
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = root;
    Ok(())
}

fn validate_native_binary(path: &Path) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("open native binary {}", path.display()))?;
    let mut header = [0_u8; 512];
    let read = file.read(&mut header)?;
    #[cfg(windows)]
    {
        ensure!(
            read >= 64 && &header[..2] == b"MZ",
            "{} is not a PE executable",
            path.display()
        );
        let offset = u32::from_le_bytes(header[0x3c..0x40].try_into().unwrap()) as usize;
        ensure!(
            offset + 6 <= read && &header[offset..offset + 4] == b"PE\0\0",
            "{} has an invalid PE header",
            path.display()
        );
        let machine = u16::from_le_bytes(header[offset + 4..offset + 6].try_into().unwrap());
        let expected = match std::env::consts::ARCH {
            "x86_64" => 0x8664,
            "aarch64" => 0xaa64,
            other => bail!("unsupported Windows architecture {other}"),
        };
        ensure!(
            machine == expected,
            "{} targets the wrong Windows architecture",
            path.display()
        );
    }
    #[cfg(target_os = "linux")]
    {
        ensure!(
            read >= 20 && &header[..4] == b"\x7fELF",
            "{} is not an ELF executable",
            path.display()
        );
        ensure!(
            header[4] == 2 && header[5] == 1,
            "{} must be a 64-bit little-endian ELF executable",
            path.display()
        );
        let machine = u16::from_le_bytes(header[18..20].try_into().unwrap());
        let expected = match std::env::consts::ARCH {
            "x86_64" => 62,
            "aarch64" => 183,
            other => bail!("unsupported Linux architecture {other}"),
        };
        ensure!(
            machine == expected,
            "{} targets the wrong Linux architecture",
            path.display()
        );
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            fs::metadata(path)?.permissions().mode() & 0o111 != 0,
            "{} is not executable",
            path.display()
        );
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = (read, header);
    Ok(())
}

fn validate_existing_install(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    ensure!(
        root.is_dir(),
        "install prefix exists and is not a directory: {}",
        root.display()
    );
    if fs::read_dir(root)?.next().is_none() {
        fs::remove_dir(root)?;
        return Ok(());
    }
    verify_installed_tree(root).with_context(|| {
        format!(
            "refusing to replace non-empty directory without a valid OJOS installation: {}",
            root.display()
        )
    })?;
    Ok(())
}

fn recover_interrupted_install(
    parent: &Path,
    install_name: &OsStr,
    install_root: &Path,
) -> Result<()> {
    let journals = read_journals(parent, install_name)?;
    let active_nonces = journals
        .iter()
        .map(|(_, journal)| journal.nonce.clone())
        .collect::<BTreeSet<_>>();
    cleanup_orphaned_stages(parent, install_name, &active_nonces)?;
    cleanup_unpublished_journal_files(parent, install_name)?;
    if journals.is_empty() {
        return Ok(());
    }
    let nonces = journals
        .iter()
        .map(|(_, journal)| journal.nonce.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        nonces.len() == 1,
        "multiple interrupted OJOS installs require manual inspection in {}",
        parent.display()
    );
    let (_, journal) = journals
        .iter()
        .max_by_key(|(_, journal)| journal.phase)
        .unwrap();
    validate_journal_paths(parent, install_name, install_root, journal)?;

    if install_root.exists() {
        if let Err(published_error) = verify_installed_tree(install_root) {
            let backup = journal.backup.as_ref().ok_or_else(|| {
                anyhow!(
                    "the interrupted first installation is incomplete and has no previous version to restore: {published_error}"
                )
            })?;
            verify_installed_tree(backup).context(
                "the published installation and its previous-version backup are both invalid",
            )?;
            let failed = parent.join(format!(
                ".{}.failed.{}",
                install_name.to_string_lossy(),
                journal.nonce
            ));
            ensure!(
                !failed.exists(),
                "failed-install evidence path already exists: {}",
                failed.display()
            );
            fs::rename(install_root, &failed)
                .context("preserve the incomplete published installation for inspection")?;
            fs::rename(backup, install_root)
                .context("restore the previous installation after incomplete publish")?;
            eprintln!(
                "warning: restored the previous OJOS installation; incomplete files remain at {}",
                failed.display()
            );
        }
        if journal.stage.exists() {
            remove_verified_stage(&journal.stage, parent)?;
        }
        if let Some(backup) = &journal.backup {
            if backup.exists() {
                remove_verified_install_tree(backup, parent)?;
            }
        }
    } else if let Some(backup) = &journal.backup {
        if backup.exists() {
            verify_installed_tree(backup).context("interrupted-install backup is invalid")?;
            fs::rename(backup, install_root).context("restore interrupted-install backup")?;
            if journal.stage.exists() {
                remove_verified_stage(&journal.stage, parent)?;
            }
        } else if journal.stage.exists() {
            verify_installed_tree(&journal.stage)
                .context("interrupted-install stage is invalid")?;
            fs::rename(&journal.stage, install_root)
                .context("publish interrupted-install stage")?;
        } else {
            bail!("interrupted install has neither a valid installation, stage, nor backup");
        }
    } else if journal.stage.exists() {
        verify_installed_tree(&journal.stage)
            .context("interrupted first-install stage is invalid")?;
        fs::rename(&journal.stage, install_root)
            .context("publish interrupted first-install stage")?;
    } else {
        bail!("interrupted first install has no recoverable stage");
    }
    remove_all_journals(parent, install_name, Some(&journal.nonce))?;
    sync_directory(parent)?;
    Ok(())
}

fn cleanup_orphaned_stages(
    parent: &Path,
    install_name: &OsStr,
    active_nonces: &BTreeSet<String>,
) -> Result<()> {
    let prefix = format!(".{}.installing.", install_name.to_string_lossy());
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(nonce) = name.strip_prefix(&prefix) else {
            continue;
        };
        if !entry.file_type()?.is_dir() || active_nonces.contains(nonce) {
            continue;
        }
        match verify_installed_tree(&entry.path()) {
            Ok(_) => remove_verified_stage(&entry.path(), parent)?,
            Err(error) => eprintln!(
                "warning: preserving incomplete orphan installer stage {} for inspection: {error}",
                entry.path().display()
            ),
        }
    }
    Ok(())
}

fn cleanup_unpublished_journal_files(parent: &Path, install_name: &OsStr) -> Result<()> {
    let prefix = format!(".{}.install-journal.", install_name.to_string_lossy());
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && name.starts_with(&prefix) && name.ends_with(".json.tmp")
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn write_journal(parent: &Path, install_name: &OsStr, journal: &InstallJournal) -> Result<PathBuf> {
    let path = parent.join(format!(
        ".{}.install-journal.{}.{}.json",
        install_name.to_string_lossy(),
        journal.nonce,
        journal.phase.file_part()
    ));
    let temporary = path.with_extension("json.tmp");
    write_json_synced(&temporary, journal)?;
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("publish installer journal {}", path.display()));
    }
    sync_directory(parent)?;
    Ok(path)
}

fn read_journals(parent: &Path, install_name: &OsStr) -> Result<Vec<(PathBuf, InstallJournal)>> {
    let prefix = format!(".{}.install-journal.", install_name.to_string_lossy());
    let mut journals = Vec::new();
    let mut invalid = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".json") {
            match read_json(&entry.path()) {
                Ok(journal) => journals.push((entry.path(), journal)),
                Err(error) => invalid.push((name, error)),
            }
        }
    }
    if journals.is_empty() && !invalid.is_empty() {
        let (name, error) = invalid.remove(0);
        return Err(error).with_context(|| {
            format!("no complete installer journal can supersede truncated journal {name}")
        });
    }
    for (name, error) in invalid {
        eprintln!(
            "warning: ignoring incomplete installer journal {name}; a prior durable phase will recover it: {error}"
        );
    }
    Ok(journals)
}

fn validate_journal_paths(
    parent: &Path,
    install_name: &OsStr,
    install_root: &Path,
    journal: &InstallJournal,
) -> Result<()> {
    ensure!(
        journal.schema_version == 1 && journal.product == PRODUCT,
        "invalid installer journal identity"
    );
    ensure!(
        path_eq(&journal.install_root, install_root),
        "installer journal targets a different prefix"
    );
    ensure_direct_child(
        parent,
        &journal.stage,
        &format!(".{}.installing.", install_name.to_string_lossy()),
    )?;
    if let Some(backup) = &journal.backup {
        ensure_direct_child(
            parent,
            backup,
            &format!(".{}.previous.", install_name.to_string_lossy()),
        )?;
    }
    Ok(())
}

fn ensure_direct_child(parent: &Path, child: &Path, prefix: &str) -> Result<()> {
    ensure!(
        child
            .parent()
            .is_some_and(|candidate| path_eq(candidate, parent)),
        "installer recovery path escapes its parent"
    );
    let name = child
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("installer recovery path is invalid"))?;
    ensure!(
        name.starts_with(prefix),
        "installer recovery path has an invalid name"
    );
    Ok(())
}

fn remove_older_journals(paths: &[PathBuf]) -> Result<()> {
    if paths.len() < 2 {
        return Ok(());
    }
    for path in &paths[..paths.len() - 1] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn remove_all_journals(parent: &Path, install_name: &OsStr, nonce: Option<&str>) -> Result<()> {
    let prefix = format!(".{}.install-journal.", install_name.to_string_lossy());
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix)
            && name.ends_with(".json")
            && nonce.is_none_or(|value| name.contains(value))
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn remove_verified_stage(path: &Path, parent: &Path) -> Result<()> {
    ensure!(
        path.parent()
            .is_some_and(|candidate| path_eq(candidate, parent)),
        "stage path escapes install parent"
    );
    verify_installed_tree(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

fn remove_verified_install_tree(path: &Path, parent: &Path) -> Result<()> {
    ensure!(
        path.parent()
            .is_some_and(|candidate| path_eq(candidate, parent)),
        "backup path escapes install parent"
    );
    verify_installed_tree(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

fn copy_manifest_files(source: &Path, destination: &Path, manifest: &BundleManifest) -> Result<()> {
    for file in &manifest.files {
        let from = safe_join(source, &file.path)?;
        let to = safe_join(destination, &file.path)?;
        copy_file_synced(&from, &to)?;
    }
    Ok(())
}

fn copy_file_synced(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("read source file {}", source.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "source is not a regular file: {}",
        source.display()
    );
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)
        .with_context(|| format!("copy {} to {}", source.display(), destination.display()))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)?
        .sync_all()?;
    Ok(())
}

fn write_bytes_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_json_synced(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_bytes_synced(path, &bytes)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_manifest_path(relative)?;
    Ok(root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

fn validate_manifest_path(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "bundle path is empty");
    ensure!(
        !path.contains('\\'),
        "bundle paths must use forward slashes: {path}"
    );
    let parsed = Path::new(path);
    ensure!(!parsed.is_absolute(), "bundle path is absolute: {path}");
    ensure!(
        parsed
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "bundle path is not normalized: {path}"
    );
    Ok(())
}

fn portable_path(path: &Path) -> Result<String> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("bundle path is not UTF-8")),
            _ => Err(anyhow!("bundle path is not relative and normalized")),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(components.join("/"))
}

fn yaml_string<'a>(value: &'a YamlValue, keys: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in keys {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn absolute_from(root: &Path, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        absolute_path(path)
    } else {
        absolute_path(&root.join(path))
    }
}

fn resolve_commit(repo_root: &Path, explicit: Option<&str>) -> Result<String> {
    let environment = std::env::var("GITHUB_SHA").ok();
    if let Some(value) = explicit.or(environment.as_deref()) {
        ensure!(
            value == "development" || is_commit_sha(value),
            "commit must be a 40-character lowercase Git SHA or development"
        );
        return Ok(value.to_string());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    ensure!(output.status.success(), "git rev-parse HEAD failed");
    let value = String::from_utf8(output.stdout)?.trim().to_string();
    ensure!(is_commit_sha(&value), "git returned an invalid commit SHA");
    Ok(value)
}

fn is_commit_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unique_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        unix_timestamp().unwrap_or_default(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn unix_timestamp() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn path_eq(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    return left
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy());
    #[cfg(not(windows))]
    return left == right;
}

fn canonical_path(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let rendered = canonical.to_string_lossy();
        if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = rendered.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(canonical)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(windows)]
mod windows_path {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, REG_SZ};

    pub fn add_to_user_path(bin: &Path) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let environment = hkcu
            .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
            .context("open HKCU\\Environment")?;
        let existing = environment.get_raw_value("Path").ok();
        let (raw_path, value_type) = match existing {
            Some(value) if value.vtype == REG_SZ || value.vtype == REG_EXPAND_SZ => {
                (decode_registry_string(&value.bytes)?, value.vtype)
            }
            Some(_) => bail!("HKCU\\Environment\\Path is not a string value"),
            None => (String::new(), REG_EXPAND_SZ),
        };
        let normalized = bin
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string();
        let present = raw_path
            .split(';')
            .filter(|entry| !entry.trim().is_empty())
            .any(|entry| {
                entry
                    .trim()
                    .trim_matches('"')
                    .trim_end_matches(['\\', '/'])
                    .eq_ignore_ascii_case(&normalized)
            });
        if present {
            return Ok(());
        }
        let updated = if raw_path.trim().is_empty() {
            normalized
        } else {
            format!("{};{}", raw_path.trim_end_matches(';'), normalized)
        };
        environment
            .set_raw_value(
                "Path",
                &winreg::RegValue {
                    bytes: encode_registry_string(&updated),
                    vtype: value_type,
                },
            )
            .context("update Windows user PATH")?;
        broadcast_environment_change();
        println!(
            "Added {} to the Windows user PATH. New terminals will see it.",
            bin.display()
        );
        Ok(())
    }

    fn decode_registry_string(bytes: &[u8]) -> Result<String> {
        ensure!(
            bytes.len() % 2 == 0,
            "Windows PATH registry value has invalid UTF-16 bytes"
        );
        let words = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|word| *word != 0)
            .collect::<Vec<_>>();
        String::from_utf16(&words).context("Windows PATH registry value is invalid UTF-16")
    }

    fn encode_registry_string(value: &str) -> Vec<u8> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    fn broadcast_environment_change() {
        let message = OsStr::new("Environment")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut result = 0_usize;
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM::default(),
                message.as_ptr() as LPARAM,
                SMTO_ABORTIFHUNG,
                5_000,
                &mut result,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_valid_install(root: &Path) {
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let current = std::env::current_exe().unwrap();
        for name in [
            "ojos-orchestrator",
            "ojos-orchestrator-daemon",
            "ojos-orchestrator-tui",
            "ojos-orchestrator-agent",
            "ojos-orchestrator-desktop",
        ] {
            fs::copy(&current, bin.join(executable_name(name))).unwrap();
        }
        #[cfg(windows)]
        fs::write(bin.join("WebView2Loader.dll"), "test loader").unwrap();
        let web = root.join("manager/web/dist");
        let schemas = root.join("platform/schemas/orchestrator");
        let service = root.join("services/example");
        fs::create_dir_all(&web).unwrap();
        fs::create_dir_all(&schemas).unwrap();
        fs::create_dir_all(&service).unwrap();
        fs::create_dir_all(root.join("store")).unwrap();
        fs::write(web.join("index.html"), "<div id=\"app\"></div>").unwrap();
        fs::write(schemas.join("actions-v1.yaml"), "schema_version: 1\n").unwrap();
        fs::write(service.join("service.yaml"), "schema_version: 1\n").unwrap();
        fs::write(
            service.join("release.yaml"),
            "schema_version: 1\nsource:\n  kind: url\n  url: docker://example@sha256:00\nmigrations: []\n",
        )
        .unwrap();
        fs::write(
            root.join("store/index.json"),
            r#"{"modules":[{"source_url":"services/example"}]}"#,
        )
        .unwrap();
        let manifest = build_manifest(root, "development").unwrap();
        write_json_synced(&root.join(BUNDLE_MANIFEST), &manifest).unwrap();
        let marker = InstallMarker {
            schema_version: 1,
            product: PRODUCT.to_string(),
            version: manifest.version,
            source_commit: "development".to_string(),
            bundle_sha256: sha256_file(&root.join(BUNDLE_MANIFEST)).unwrap(),
            installed_at: "0".to_string(),
        };
        write_json_synced(&root.join(INSTALL_MARKER), &marker).unwrap();
        verify_installed_tree(root).unwrap();
    }

    #[test]
    fn manifest_paths_are_portable_and_cannot_escape() {
        for valid in [
            "bin/ojos-orchestrator",
            "services/auth-service/release.yaml",
        ] {
            validate_manifest_path(valid).unwrap();
        }
        for invalid in ["", "../secret", "a/../b", "/absolute", "a\\b", "./a"] {
            assert!(
                validate_manifest_path(invalid).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn commit_identity_is_strict() {
        assert!(is_commit_sha("1ac9ddbae75dddcb7a6d94b6f9bc1832cde200e0"));
        assert!(!is_commit_sha("1AC9DDBAE75DDDCB7A6D94B6F9BC1832CDE200E0"));
        assert!(!is_commit_sha("main"));
    }

    #[test]
    fn installer_lock_allows_only_one_of_32_concurrent_claims() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("install.lock");
        File::create(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(32));
        let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let threads = (0..32)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                let winners = winners.clone();
                std::thread::spawn(move || {
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .unwrap();
                    barrier.wait();
                    if file.try_lock_exclusive().is_ok() {
                        winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        file.unlock().unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn running_installed_process_blocks_upgrade_and_upgrade_blocks_start() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("OJOS runtime");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let executable = bin.join(executable_name("ojos-orchestrator-daemon"));
        fs::write(&executable, "runtime").unwrap();
        let marker = InstallMarker {
            schema_version: 1,
            product: PRODUCT.to_string(),
            version: "1.0.0".to_string(),
            source_commit: "development".to_string(),
            bundle_sha256: "digest".to_string(),
            installed_at: "0".to_string(),
        };
        write_json_synced(&root.join(INSTALL_MARKER), &marker).unwrap();
        let lock_path = directory.path().join(".OJOS runtime.install.lock");

        let runtime = acquire_runtime_install_guard_from(&executable).unwrap();
        let installer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert!(installer.try_lock_exclusive().is_err());
        drop(runtime);
        installer.try_lock_exclusive().unwrap();
        assert!(acquire_runtime_install_guard_from(&executable).is_err());
        installer.unlock().unwrap();
        acquire_runtime_install_guard_from(&executable).unwrap();
    }

    #[test]
    fn runtime_root_discovery_includes_local_sources_and_migrations() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let roots = discover_runtime_roots(&repository).unwrap();
        for expected in [
            "services/auth-service",
            "services/gateway",
            "services/judge-api",
            "services/judge-worker",
            "services/problem-service",
            "services/storage-service",
            "services/user-service",
            "services/orchestrator/migrations/000001_orchestrator_schema.up.sql",
            "platform/shared/go",
        ] {
            assert!(
                roots.iter().any(|root| root == expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn interrupted_first_install_publishes_a_verified_stage() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path();
        let install_name = OsStr::new("OJOS install");
        let root = parent.join(install_name);
        let stage = parent.join(".OJOS install.installing.test-first");
        create_valid_install(&stage);
        let journal = InstallJournal {
            schema_version: 1,
            product: PRODUCT.to_string(),
            nonce: "test-first".to_string(),
            phase: JournalPhase::Prepared,
            install_root: root.clone(),
            stage,
            backup: None,
        };
        write_journal(parent, install_name, &journal).unwrap();

        recover_interrupted_install(parent, install_name, &root).unwrap();

        verify_installed_tree(&root).unwrap();
        assert!(read_journals(parent, install_name).unwrap().is_empty());
    }

    #[test]
    fn invalid_published_upgrade_restores_the_verified_backup() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path();
        let install_name = OsStr::new("OJOS 空格");
        let root = parent.join(install_name);
        let backup = parent.join(".OJOS 空格.previous.test-upgrade");
        let stage = parent.join(".OJOS 空格.installing.test-upgrade");
        create_valid_install(&backup);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("corrupt"), "incomplete publish").unwrap();
        let journal = InstallJournal {
            schema_version: 1,
            product: PRODUCT.to_string(),
            nonce: "test-upgrade".to_string(),
            phase: JournalPhase::OldMoved,
            install_root: root.clone(),
            stage,
            backup: Some(backup.clone()),
        };
        write_journal(parent, install_name, &journal).unwrap();

        recover_interrupted_install(parent, install_name, &root).unwrap();

        verify_installed_tree(&root).unwrap();
        assert!(!backup.exists());
        assert!(
            parent
                .join(".OJOS 空格.failed.test-upgrade/corrupt")
                .is_file()
        );
        assert!(read_journals(parent, install_name).unwrap().is_empty());
    }

    #[test]
    fn truncated_newer_journal_falls_back_to_the_last_durable_phase() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path();
        let install_name = OsStr::new("OJOS journal");
        let root = parent.join(install_name);
        let backup = parent.join(".OJOS journal.previous.test-truncated");
        let stage = parent.join(".OJOS journal.installing.test-truncated");
        create_valid_install(&backup);
        create_valid_install(&stage);
        let journal = InstallJournal {
            schema_version: 1,
            product: PRODUCT.to_string(),
            nonce: "test-truncated".to_string(),
            phase: JournalPhase::Prepared,
            install_root: root.clone(),
            stage: stage.clone(),
            backup: Some(backup.clone()),
        };
        let durable = write_journal(parent, install_name, &journal).unwrap();
        assert!(!durable.with_extension("json.tmp").exists());
        fs::write(
            parent.join(".OJOS journal.install-journal.test-truncated.old-moved.json"),
            "{\"truncated\":",
        )
        .unwrap();

        recover_interrupted_install(parent, install_name, &root).unwrap();

        verify_installed_tree(&root).unwrap();
        assert!(!backup.exists());
        assert!(!stage.exists());
        assert!(read_journals(parent, install_name).unwrap().is_empty());
    }

    #[test]
    fn prepublish_crash_cleans_verified_orphan_stage_and_journal_temp() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path();
        let install_name = OsStr::new("OJOS orphan");
        let root = parent.join(install_name);
        let stage = parent.join(".OJOS orphan.installing.test-orphan");
        create_valid_install(&root);
        create_valid_install(&stage);
        let journal_temp =
            parent.join(".OJOS orphan.install-journal.test-orphan.prepared.json.tmp");
        fs::write(&journal_temp, "{\"partial\":").unwrap();

        recover_interrupted_install(parent, install_name, &root).unwrap();

        verify_installed_tree(&root).unwrap();
        assert!(!stage.exists());
        assert!(!journal_temp.exists());
    }
}
