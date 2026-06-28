use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use service_installer_core::{
    Manifest, Plan, RegistrySnapshot, ServiceDecl, WorkerDecl, disable_plan, enable_plan,
    expand_set, install_plan, package_module, package_service, service_install_plan,
    uninstall_plan, validate_endpoint_id, validate_manifest, validate_manifest_file,
    validate_service_manifest_file, validate_service_set_file, verify_package,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration as StdDuration, Instant};
use uuid::Uuid;

const DEFAULT_COMPOSE_FILE: &str = "deploy/compose/docker-compose.yml";
const DEFAULT_ENV_FILE: &str = ".env";
const DEFAULT_OPERATION_LOG: &str = ".tmp/agent/runtime-operations.jsonl";
const DEFAULT_LOCK_DIR: &str = ".tmp/agent/runtime-locks";
const APPLY_TIMEOUT_SECONDS: u64 = 60;
const OUTPUT_LIMIT: usize = 4096;

#[derive(Parser)]
#[command(name = "ojosctl")]
#[command(about = "OJOS Root Runtime Manager CLI")]
#[command(version)]
struct Cli {
    #[arg(long, global = true, help = "Output JSON for scripts and CI")]
    json: bool,
    #[arg(
        long,
        global = true,
        help = "Show controlled paths and execution details"
    )]
    verbose: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Doctor {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Status {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long, default_value = DEFAULT_OPERATION_LOG)]
        operation_log: PathBuf,
    },
    Module {
        #[command(subcommand)]
        command: ModuleCommands,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    Set {
        #[command(subcommand)]
        command: SetCommands,
    },
    Endpoint {
        #[command(subcommand)]
        command: EndpointCommands,
    },
    Link {
        #[command(subcommand)]
        command: LinkCommands,
    },
    Topology {
        #[command(subcommand)]
        command: TopologyCommands,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },
}

#[derive(Subcommand)]
enum ServiceCommands {
    Discover {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Validate {
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    InstallPlan {
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Package {
        service_dir: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    Verify {
        package: PathBuf,
    },
    Enable {
        service_id: String,
    },
    Disable {
        service_id: String,
    },
}

#[derive(Subcommand)]
enum SetCommands {
    List {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Validate {
        set: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Expand {
        set: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
}

#[derive(Subcommand)]
enum EndpointCommands {
    Validate {
        endpoint: String,
    },
    PlanRegister {
        service_id: String,
        endpoint: String,
    },
}

#[derive(Subcommand)]
enum LinkCommands {
    PlanCreate {
        source: String,
        target: String,
        #[arg(long, default_value = "http")]
        protocol: String,
        #[arg(long, default_value = "internal")]
        auth_mode: String,
    },
}

#[derive(Subcommand)]
enum TopologyCommands {
    Snapshot {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
}

#[derive(Subcommand)]
enum ModuleCommands {
    Init {
        module_id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "feature")]
        kind: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long, default_value = "metadata")]
        with_service: String,
        #[arg(long, default_value = "disabled")]
        with_gateway_route: String,
        #[arg(long, default_value = "disabled")]
        with_menu: String,
        #[arg(long)]
        with_topology: bool,
    },
    Discover {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Validate {
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Plan {
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Package {
        module_dir: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    Verify {
        package: PathBuf,
    },
    Inspect {
        package: PathBuf,
    },
    Doctor {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    InstallPlan {
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Install {
        manifest: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        confirm: bool,
    },
    Enable {
        module_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        confirm: bool,
    },
    Disable {
        module_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        confirm: bool,
    },
    UninstallDryRun {
        module_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
}

#[derive(Subcommand)]
enum RuntimeCommands {
    Snapshot {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Routes {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Services {
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    Service {
        service_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    PlanStart {
        service_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    PlanStop {
        service_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    PlanRestart {
        service_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    ApplyPlan {
        plan: PathBuf,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long, default_value = DEFAULT_OPERATION_LOG)]
        operation_log: PathBuf,
        #[arg(long)]
        verbose: bool,
    },
    Operations {
        #[arg(long, default_value = DEFAULT_OPERATION_LOG)]
        operation_log: PathBuf,
    },
    Operation {
        operation_id: String,
        #[arg(long, default_value = DEFAULT_OPERATION_LOG)]
        operation_log: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output = OutputMode {
        json: cli.json,
        verbose: cli.verbose,
    };
    match cli.command {
        Commands::Doctor { repo_root } => run_doctor(&repo_root, &output),
        Commands::Status {
            repo_root,
            operation_log,
        } => run_status(&repo_root, &operation_log, &output),
        Commands::Module { command } => run_module(command, &output),
        Commands::Service { command } => run_service(command, &output),
        Commands::Set { command } => run_set(command, &output),
        Commands::Endpoint { command } => run_endpoint(command, &output),
        Commands::Link { command } => run_link(command, &output),
        Commands::Topology { command } => run_topology(command, &output),
        Commands::Runtime { command } => run_runtime(command, &output),
    }
}

#[derive(Copy, Clone)]
struct OutputMode {
    json: bool,
    verbose: bool,
}

fn run_service(command: ServiceCommands, output: &OutputMode) -> Result<()> {
    match command {
        ServiceCommands::Discover { repo_root } => {
            let services = discover_service_manifests(&repo_root)?;
            let count = services.len();
            print_value(
                &serde_json::json!({ "services": services }),
                output,
                "Service discovery completed",
                vec![format!("found {} service.yaml files", count)],
            )
        }
        ServiceCommands::Validate {
            manifest,
            repo_root,
        } => {
            let manifest = validate_service_manifest_file(&repo_root, &manifest)?;
            let service_id = manifest.id.clone();
            let version = manifest.version.clone();
            let port = manifest.endpoint.default_port;
            print_value(
                &serde_json::json!({ "valid": true, "service": manifest }),
                output,
                "Service contract valid",
                vec![
                    format!("Service: {}", service_id),
                    format!("version: {}", version),
                    format!("default_port: {}", port),
                ],
            )
        }
        ServiceCommands::InstallPlan {
            manifest,
            repo_root,
        } => {
            let manifest = validate_service_manifest_file(&repo_root, &manifest)?;
            let plan = service_install_plan(&manifest, &[]);
            print_value(
                &plan,
                output,
                "Service install plan",
                vec![
                    format!("Service: {}", plan.service_id),
                    format!("version: {}", plan.version),
                    format!("actions: {}", plan.actions.len()),
                    format!("can_apply: {}", plan.can_apply),
                ],
            )
        }
        ServiceCommands::Package {
            service_dir,
            output: package_output,
        } => {
            let result = package_service(&service_dir, &package_output)?;
            print_value(
                &result,
                output,
                "Service package created",
                vec![
                    format!("Service: {}", result.service_id),
                    format!("version: {}", result.version),
                    format!("files: {}", result.files_checked),
                ],
            )
        }
        ServiceCommands::Verify { package } => {
            let result = verify_package(&package)?;
            print_value(
                &result,
                output,
                "Service package verified",
                vec![
                    format!("Service: {}", result.service_id),
                    format!("version: {}", result.version),
                    format!("files: {}", result.files_checked),
                ],
            )
        }
        ServiceCommands::Enable { service_id } => print_value(
            &serde_json::json!({
                "service_id": &service_id,
                "action": "enable",
                "status": "planned",
                "note": "Root Runtime Manager applies service enable operations"
            }),
            output,
            "Service enable plan",
            vec![format!("Service: {}", service_id)],
        ),
        ServiceCommands::Disable { service_id } => print_value(
            &serde_json::json!({
                "service_id": &service_id,
                "action": "disable",
                "status": "planned",
                "note": "Root Runtime Manager applies service disable operations"
            }),
            output,
            "Service disable plan",
            vec![format!("Service: {}", service_id)],
        ),
    }
}

fn run_set(command: SetCommands, output: &OutputMode) -> Result<()> {
    match command {
        SetCommands::List { repo_root } => {
            let sets = discover_sets(&repo_root)?;
            let count = sets.len();
            print_value(
                &serde_json::json!({ "sets": sets }),
                output,
                "Set list",
                vec![format!("sets: {}", count)],
            )
        }
        SetCommands::Validate { set, repo_root } => {
            let set = validate_service_set_file(&repo_root, &set)?;
            let set_id = set.id.clone();
            let service_count = set.services.len();
            print_value(
                &serde_json::json!({ "valid": true, "set": set }),
                output,
                "Set valid",
                vec![
                    format!("Set: {}", set_id),
                    format!("services: {}", service_count),
                ],
            )
        }
        SetCommands::Expand { set, repo_root } => {
            let set = validate_service_set_file(&repo_root, &set)?;
            let expanded = expand_set(&set);
            print_value(
                &expanded,
                output,
                "Set expanded",
                vec![
                    format!("Set: {}", expanded.set_id),
                    format!("services: {}", expanded.services.len()),
                    format!("default_links: {}", expanded.default_links.len()),
                ],
            )
        }
    }
}

fn run_endpoint(command: EndpointCommands, output: &OutputMode) -> Result<()> {
    match command {
        EndpointCommands::Validate { endpoint } => {
            validate_endpoint_id(&endpoint)?;
            print_value(
                &serde_json::json!({ "valid": true, "endpoint": &endpoint }),
                output,
                "Endpoint valid",
                vec![format!("Endpoint: {}", endpoint)],
            )
        }
        EndpointCommands::PlanRegister {
            service_id,
            endpoint,
        } => {
            validate_endpoint_id(&endpoint)?;
            print_value(
                &serde_json::json!({
                    "action": "register_endpoint",
                    "service_id": &service_id,
                    "endpoint": &endpoint,
                    "primary_id": &endpoint
                }),
                output,
                "Endpoint register plan",
                vec![format!("{} -> {}", service_id, endpoint)],
            )
        }
    }
}

fn run_link(command: LinkCommands, output: &OutputMode) -> Result<()> {
    match command {
        LinkCommands::PlanCreate {
            source,
            target,
            protocol,
            auth_mode,
        } => {
            validate_endpoint_id(&source)?;
            validate_endpoint_id(&target)?;
            print_value(
                &serde_json::json!({
                    "action": "create_link",
                    "source": &source,
                    "target": &target,
                    "primary_id": format!("{} -> {}", source, target),
                    "protocol": &protocol,
                    "auth_mode": &auth_mode
                }),
                output,
                "Link create plan",
                vec![format!("{} -> {}", source, target)],
            )
        }
    }
}

fn run_topology(command: TopologyCommands, output: &OutputMode) -> Result<()> {
    match command {
        TopologyCommands::Snapshot { repo_root } => {
            let services = discover_service_manifests(&repo_root)?;
            let sets = discover_sets(&repo_root)?;
            let endpoints = services
                .iter()
                .filter_map(|item| item.get("default_endpoint").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let value = serde_json::json!({
                "devices": [{ "device_id": "root-local", "kind": "root" }],
                "services": services,
                "sets": sets,
                "endpoints": endpoints,
                "links": [],
                "views": ["set", "service", "endpoint", "link", "device", "health"]
            });
            print_value(
                &value,
                output,
                "Topology Snapshot",
                vec![
                    format!(
                        "Service count: {}",
                        value["services"].as_array().map(|v| v.len()).unwrap_or(0)
                    ),
                    format!(
                        "Set count: {}",
                        value["sets"].as_array().map(|v| v.len()).unwrap_or(0)
                    ),
                ],
            )
        }
    }
}

fn discover_service_manifests(repo_root: &Path) -> Result<Vec<serde_json::Value>> {
    let services_dir = repo_root.join("services");
    let mut items = Vec::new();
    if services_dir.exists() {
        for entry in fs::read_dir(&services_dir).context("read services directory")? {
            let entry = entry.context("read service entry")?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest_path = PathBuf::from("services")
                .join(entry.file_name())
                .join("service.yaml");
            if repo_root.join(&manifest_path).exists() {
                match validate_service_manifest_file(repo_root, &manifest_path) {
                    Ok(manifest) => items.push(serde_json::json!({
                        "manifest_path": slash_path(&manifest_path),
                        "service_id": manifest.id,
                        "name": manifest.name,
                        "version": manifest.version,
                        "kind": manifest.kind,
                        "default_endpoint": format!("0.0.0.0:{}", manifest.endpoint.default_port),
                        "valid": true
                    })),
                    Err(err) => items.push(serde_json::json!({
                        "manifest_path": slash_path(&manifest_path),
                        "valid": false,
                        "error": err.to_string()
                    })),
                }
            }
        }
    }
    Ok(items)
}

fn discover_sets(repo_root: &Path) -> Result<Vec<serde_json::Value>> {
    let sets_dir = repo_root.join("sets");
    let mut items = Vec::new();
    if sets_dir.exists() {
        for entry in fs::read_dir(&sets_dir).context("read sets directory")? {
            let entry = entry.context("read set entry")?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let set_path = PathBuf::from("sets").join(entry.file_name());
            if set_path.extension().and_then(|v| v.to_str()) != Some("yaml") {
                continue;
            }
            match validate_service_set_file(repo_root, &set_path) {
                Ok(set) => items.push(serde_json::json!({
                    "set_path": slash_path(&set_path),
                    "set_id": set.id,
                    "name": set.name,
                    "services": set.services,
                    "default_links": set.default_links,
                    "non_root_only": set.non_root_only,
                    "valid": true
                })),
                Err(err) => items.push(serde_json::json!({
                    "set_path": slash_path(&set_path),
                    "valid": false,
                    "error": err.to_string()
                })),
            }
        }
    }
    Ok(items)
}

fn run_module(command: ModuleCommands, output: &OutputMode) -> Result<()> {
    eprintln!(
        "legacy compatibility: `ojosctl module` is only for old module.yaml; prefer `ojosctl service`."
    );
    match command {
        ModuleCommands::Init {
            module_id,
            name,
            kind,
            out,
            force,
            with_service,
            with_gateway_route,
            with_menu,
            with_topology,
        } => scaffold_module(
            ModuleScaffoldOptions {
                module_id,
                name,
                kind,
                out,
                force,
                with_service,
                with_gateway_route,
                with_menu,
                with_topology,
            },
            output,
        ),
        ModuleCommands::Discover { repo_root } => {
            let modules_dir = repo_root.join("modules");
            let mut items = Vec::new();
            if modules_dir.exists() {
                for entry in fs::read_dir(&modules_dir).context("read modules directory")? {
                    let entry = entry.context("read module entry")?;
                    let manifest_path = PathBuf::from("modules")
                        .join(entry.file_name())
                        .join("module.yaml");
                    if repo_root.join(&manifest_path).exists() {
                        match validate_manifest_file(&repo_root, &manifest_path) {
                            Ok(manifest) => items.push(serde_json::json!({
                                "manifest_path": slash_path(&manifest_path),
                                "module_id": manifest.id,
                                "name": manifest.name,
                                "version": manifest.version,
                                "valid": true
                            })),
                            Err(err) => items.push(serde_json::json!({
                                "manifest_path": slash_path(&manifest_path),
                                "valid": false,
                                "error": err.to_string()
                            })),
                        }
                    }
                }
            }
            print_value(
                &serde_json::json!({ "modules": items }),
                output,
                "legacy module discover completed",
                vec![format!("found {} legacy module.yaml files", items.len())],
            )
        }
        ModuleCommands::Validate {
            manifest,
            repo_root,
        } => {
            let manifest = validate_manifest_file(&repo_root, &manifest)?;
            validate_manifest(&manifest)?;
            print_value(
                &serde_json::json!({ "valid": true, "manifest": manifest }),
                output,
                "Manifest 鏍￠獙閫氳繃",
                vec![
                    format!("妯″潡: {}", manifest.id),
                    format!("鐗堟湰: {}", manifest.version),
                ],
            )
        }
        ModuleCommands::Plan {
            manifest,
            repo_root,
        }
        | ModuleCommands::InstallPlan {
            manifest,
            repo_root,
        } => {
            let manifest = validate_manifest_file(&repo_root, &manifest)?;
            let plan = install_plan(&manifest, &RegistrySnapshot::default(), true)?;
            print_plan(&plan, output)
        }
        ModuleCommands::Package {
            module_dir,
            output: package_output,
        } => {
            let result = package_module(&module_dir, &package_output)?;
            print_value(
                &result,
                output,
                "妯″潡鍖呭凡鐢熸垚",
                vec![
                    format!("妯″潡: {}", result.module_id),
                    format!("鐗堟湰: {}", result.version),
                    format!("鏂囦欢鏁? {}", result.files_checked),
                ],
            )
        }
        ModuleCommands::Verify { package } | ModuleCommands::Inspect { package } => {
            let result = verify_package(&package)?;
            print_value(
                &result,
                output,
                "妯″潡鍖呴獙璇侀€氳繃",
                vec![
                    format!("妯″潡: {}", result.module_id),
                    format!("鐗堟湰: {}", result.version),
                    format!("鏂囦欢鏁? {}", result.files_checked),
                ],
            )
        }
        ModuleCommands::Doctor { repo_root } => run_doctor(&repo_root, output),
        ModuleCommands::Install {
            manifest,
            dry_run,
            repo_root,
            confirm,
        } => module_install_command(&manifest, &repo_root, dry_run, confirm, output),
        ModuleCommands::Enable {
            module_id,
            repo_root,
            confirm,
        } => module_state_plan_command(&repo_root, &module_id, "enable", confirm, output),
        ModuleCommands::Disable {
            module_id,
            repo_root,
            confirm,
        } => module_state_plan_command(&repo_root, &module_id, "disable", confirm, output),
        ModuleCommands::UninstallDryRun {
            module_id,
            repo_root,
        } => {
            let snapshot = local_registry_snapshot(&repo_root)?;
            let plan = uninstall_plan(&module_id, &snapshot, true)?;
            print_plan(&plan, output)
        }
    }
}

fn run_doctor(repo_root: &Path, output: &OutputMode) -> Result<()> {
    let modules_dir = repo_root.join("modules");
    let compose_file = repo_root.join(DEFAULT_COMPOSE_FILE);
    let sample_manifest = repo_root.join("modules/sample-hello/module.yaml");
    let value = serde_json::json!({
        "ok": modules_dir.is_dir() && compose_file.is_file() && sample_manifest.is_file(),
        "repo_root": if output.verbose { slash_path(repo_root) } else { ".".to_string() },
        "modules_dir_exists": modules_dir.is_dir(),
        "compose_file_exists": compose_file.is_file(),
        "sample_manifest_exists": sample_manifest.is_file(),
        "manifest_schema_versions": [1],
        "runtime_snapshot_version": 1,
        "package": {
            "format": "ojosmod",
            "version": 1,
            "checksum_integrity": true,
            "signature_trust_policy": "not_complete"
        },
        "native_installers": {
            "cli": "ojosctl",
            "tui": "ojos-installer-tui"
        }
    });
    print_value(
        &value,
        output,
        "OJOS doctor 瀹屾垚",
        vec![
            format!("modules 鐩綍: {}", ok_text(modules_dir.is_dir())),
            format!("compose 鏂囦欢: {}", ok_text(compose_file.is_file())),
            "瀹樻柟瀹夎鍏ュ彛: ojosctl / ojos-installer-tui".to_string(),
        ],
    )?;
    if !modules_dir.is_dir() {
        bail!("modules directory is missing");
    }
    if !compose_file.is_file() {
        bail!("trusted compose file is missing");
    }
    Ok(())
}

fn run_status(repo_root: &Path, operation_log: &Path, output: &OutputMode) -> Result<()> {
    let services = load_runtime_services(repo_root)?;
    let operations = read_operation_log(operation_log).unwrap_or_default();
    let modules = discover_local_modules(repo_root)?;
    let blocked = services
        .iter()
        .filter(|service| !service.blocked_by.is_empty())
        .count();
    let value = serde_json::json!({
        "ok": blocked == 0,
        "modules": modules.len(),
        "runtime_services": services.len(),
        "blocked_runtime_services": blocked,
        "operations": operations.len(),
        "gateway_apply": "disabled",
        "official_installer": ["ojosctl", "ojos-installer-tui"]
    });
    print_value(
        &value,
        output,
        "OJOS status",
        vec![
            format!("legacy modules: {}", modules.len()),
            format!("runtime services: {}", services.len()),
            format!("blocked runtime services: {}", blocked),
            "Gateway/Web apply: disabled".to_string(),
        ],
    )
}

fn discover_local_modules(repo_root: &Path) -> Result<Vec<Manifest>> {
    let mut out = Vec::new();
    let modules_dir = repo_root.join("modules");
    if !modules_dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(&modules_dir).context("read modules directory")? {
        let entry = entry.context("read module entry")?;
        let manifest_path = PathBuf::from("modules")
            .join(entry.file_name())
            .join("module.yaml");
        if repo_root.join(&manifest_path).exists() {
            out.push(validate_manifest_file(repo_root, &manifest_path)?);
        }
    }
    out.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(out)
}

fn local_registry_snapshot(repo_root: &Path) -> Result<RegistrySnapshot> {
    let modules = discover_local_modules(repo_root)?
        .into_iter()
        .map(|manifest| service_installer_core::InstalledModule {
            module_id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            status: if manifest.status == "builtin" {
                service_installer_core::ModuleState::Enabled
            } else {
                service_installer_core::ModuleState::Installed
            },
            kind: manifest.kind.clone(),
            manifest: Some(manifest),
        })
        .collect();
    Ok(RegistrySnapshot { modules })
}

fn module_install_command(
    manifest_path: &Path,
    repo_root: &Path,
    dry_run: bool,
    confirm: bool,
    output: &OutputMode,
) -> Result<()> {
    let manifest = validate_manifest_file(repo_root, manifest_path)?;
    let snapshot = local_registry_snapshot(repo_root)?;
    let plan = install_plan(&manifest, &snapshot, true)?;
    if dry_run || !confirm {
        print_plan(&plan, output)?;
        if !dry_run && !confirm {
            bail!("module install apply requires --confirm; use --dry-run for plan-only");
        }
        return Ok(());
    }
    bail!("legacy module install apply must use Root Runtime Manager controlled path")
}

fn module_state_plan_command(
    repo_root: &Path,
    module_id: &str,
    action: &str,
    confirm: bool,
    output: &OutputMode,
) -> Result<()> {
    let snapshot = local_registry_snapshot(repo_root)?;
    let plan = match action {
        "enable" => enable_plan(module_id, &snapshot, !confirm)?,
        "disable" => disable_plan(module_id, &snapshot, !confirm)?,
        _ => bail!("unsupported module action {}", action),
    };
    print_plan(&plan, output)?;
    if confirm {
        bail!(
            "{} apply must use Root Runtime Manager controlled operation history",
            action
        );
    }
    Ok(())
}

fn runtime_snapshot_value(repo_root: &Path) -> Result<Value> {
    let manifests = discover_local_modules(repo_root)?;
    let services = load_runtime_services(repo_root)?;
    let routes = manifests
        .iter()
        .flat_map(|manifest| {
            manifest
                .provides
                .gateway_routes
                .iter()
                .map(|route| {
                    serde_json::json!({
                        "module_id": manifest.id,
                        "prefix": route.prefix,
                        "service_id": route.target_service,
                        "auth_mode": route.auth_mode,
                        "enabled": route.enabled
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "version": 1,
        "modules": manifests.iter().map(|manifest| serde_json::json!({
            "module_id": manifest.id,
            "name": manifest.name,
            "version": manifest.version,
            "kind": manifest.kind,
            "status": manifest.status
        })).collect::<Vec<_>>(),
        "services": services.iter().filter(|item| item.kind != "worker").collect::<Vec<_>>(),
        "workers": services.iter().filter(|item| item.kind == "worker").collect::<Vec<_>>(),
        "routes": routes
    }))
}

fn runtime_routes_value(repo_root: &Path) -> Result<Value> {
    let manifests = discover_local_modules(repo_root)?;
    let mut routes = Vec::new();
    for manifest in manifests {
        for route in manifest.provides.gateway_routes {
            routes.push(serde_json::json!({
                "module_id": manifest.id,
                "prefix": route.prefix,
                "service_id": route.target_service,
                "auth_mode": route.auth_mode,
                "enabled": route.enabled,
                "status": if route.enabled { "declared" } else { "disabled" }
            }));
        }
    }
    Ok(serde_json::json!({ "routes": routes }))
}

struct ModuleScaffoldOptions {
    module_id: String,
    name: String,
    kind: String,
    out: PathBuf,
    force: bool,
    with_service: String,
    with_gateway_route: String,
    with_menu: String,
    with_topology: bool,
}

fn scaffold_module(opts: ModuleScaffoldOptions, output: &OutputMode) -> Result<()> {
    validate_scaffold_options(&opts)?;
    if opts.out.exists() && !opts.force {
        bail!("output directory already exists; pass --force to replace scaffold files");
    }
    fs::create_dir_all(&opts.out)
        .with_context(|| format!("create module directory {}", slash_path(&opts.out)))?;
    for rel in ["migrations", "frontend", "services", "tests"] {
        fs::create_dir_all(opts.out.join(rel))
            .with_context(|| format!("create {}", slash_path(&opts.out.join(rel))))?;
    }

    let slug = module_slug(&opts.module_id);
    let permission = format!("{}.view", slug.replace('-', "."));
    let service_id = format!("{}-metadata-service", slug);
    let component_id = format!("{}-component", slug);
    let health_id = format!("{}-health", slug);
    let bucket_id = format!("{}-metadata", slug);
    let route_path = format!("/admin/modules/{}", slug);
    let gateway_prefix = format!("/api/{}", slug);
    let manifest = sample_manifest_text(
        &opts.module_id,
        &opts.name,
        &opts.kind,
        &slug,
        &permission,
        &service_id,
        &component_id,
        &health_id,
        &bucket_id,
        &route_path,
        &gateway_prefix,
        &opts.with_service,
        &opts.with_gateway_route,
        &opts.with_menu,
        opts.with_topology,
    );

    write_scaffold_file(&opts.out.join("module.yaml"), &manifest, opts.force)?;
    write_scaffold_file(
        &opts.out.join("README.md"),
        &format!(
            "# {}\n\nMetadata-only OJOS sample module generated by `ojosctl module init`.\n\nThis scaffold does not contain hooks, scripts, dynamic frontend bundles, host mounts or executable runtime instructions.\n",
            opts.name
        ),
        opts.force,
    )?;
    write_scaffold_file(
        &opts.out.join("frontend/contributions.yaml"),
        "# Frontend contribution metadata only. Web Shell does not execute dynamic JavaScript from this directory.\n",
        opts.force,
    )?;
    write_scaffold_file(
        &opts.out.join("services/README.md"),
        "# Services\n\nThis scaffold defaults to metadata-only services. Add real services only after registering a trusted runtime driver and deploy allowlist.\n",
        opts.force,
    )?;
    write_scaffold_file(
        &opts.out.join("tests/module-smoke.md"),
        "# Module Smoke\n\n- Run `ojosctl module validate modules/<module>/module.yaml`.\n- Run `ojosctl module package modules/<module> -o .tmp/agent/scratch/<module>.ojosmod`.\n- Install, enable, inspect runtime snapshot, then disable.\n",
        opts.force,
    )?;

    let manifest_path = opts.out.join("module.yaml");
    let manifest: Manifest = serde_yaml::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", slash_path(&manifest_path)))?,
    )
    .context("parse generated manifest")?;
    validate_manifest(&manifest)?;
    print_value(
        &serde_json::json!({
            "created": true,
            "module_id": manifest.id,
            "name": manifest.name,
            "path": slash_path(&opts.out),
            "manifest_path": slash_path(&manifest_path),
            "valid": true
        }),
        output,
        "legacy module scaffold generated",
        vec![
            format!("legacy module: {}", manifest.id),
            format!("directory: {}", slash_path(&opts.out)),
            "metadata-only by default; no hook/script/dynamic bundle".to_string(),
        ],
    )
}

fn validate_scaffold_options(opts: &ModuleScaffoldOptions) -> Result<()> {
    let id_re = regex::Regex::new(r"^[a-z0-9][a-z0-9.-]*$").expect("valid regex");
    if !id_re.is_match(opts.module_id.trim()) {
        bail!("module id format is invalid");
    }
    if opts.name.trim().is_empty() {
        bail!("--name is required");
    }
    if !matches!(
        opts.kind.as_str(),
        "feature" | "integration" | "metadata" | "platform" | "kernel"
    ) {
        bail!("--kind is invalid");
    }
    if !matches!(opts.with_service.as_str(), "metadata" | "none") {
        bail!("--with-service supports metadata or none");
    }
    if !matches!(opts.with_gateway_route.as_str(), "disabled" | "none") {
        bail!("--with-gateway-route supports disabled or none");
    }
    if !matches!(opts.with_menu.as_str(), "disabled" | "none") {
        bail!("--with-menu supports disabled or none");
    }
    Ok(())
}

fn write_scaffold_file(path: &Path, text: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!("refusing to overwrite {}", slash_path(path));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text).with_context(|| format!("write {}", slash_path(path)))
}

fn module_slug(module_id: &str) -> String {
    module_id
        .trim()
        .trim_start_matches("ojos.")
        .replace('.', "-")
}

#[allow(clippy::too_many_arguments)]
fn sample_manifest_text(
    module_id: &str,
    name: &str,
    kind: &str,
    slug: &str,
    permission: &str,
    service_id: &str,
    component_id: &str,
    health_id: &str,
    bucket_id: &str,
    route_path: &str,
    gateway_prefix: &str,
    with_service: &str,
    with_gateway_route: &str,
    with_menu: &str,
    with_topology: bool,
) -> String {
    let service_block = if with_service == "metadata" {
        format!(
            r#"
  services:
    - id: {service_id}
      name: {name} Metadata Service
      kind: metadata
      lifecycle: metadata
      trusted_runtime: metadata
      health_check_id: {health_id}
      routes: []
      required: false
"#
        )
    } else {
        "\n  services: []\n".to_string()
    };
    let menu_block = if with_menu == "disabled" {
        format!(
            r#"
  menus:
    - key: {slug}
      title: {name}
      route_path: {route_path}
      sort_order: 950
      required_permission: {permission}
      enabled: false
"#
        )
    } else {
        "\n  menus: []\n".to_string()
    };
    let gateway_block = if with_gateway_route == "disabled" {
        format!(
            r#"
  gateway_routes:
    - prefix: {gateway_prefix}
      service_id: {slug}-api
      auth_mode: user
      enabled: false
"#
        )
    } else {
        "\n  gateway_routes: []\n".to_string()
    };
    let topology_block = if with_topology {
        format!(
            r#"
  topology:
    nodes:
      - id: {component_id}
        type: metadata
        label: {name} Component
      - id: {health_id}
        type: health
        label: {name} Metadata Health
    edges:
      - from: {component_id}
        to: {health_id}
        type: observes
"#
        )
    } else {
        "\n  topology:\n    nodes: []\n    edges: []\n".to_string()
    };

    format!(
        r#"schema_version: 1
id: {module_id}
name: {name}
version: 0.1.0
set: sdk-sample
kind: {kind}
status: demo
description: Metadata-only OJOS module scaffold generated by ojosctl.

compatibility:
  platform: ">=0.1.0"
  installer: ">=0.1.0"

requires:
  modules:
    - id: ojos.platform.web-shell
      version: ">=0.1.0"
    - id: ojos.platform.identity-access
      version: ">=0.1.0"
    - id: ojos.kernel.module-runtime
      version: ">=0.1.0"

provides:
  permissions:
    - key: {permission}
      description: View {name} metadata.

  roles: []

  components:
    - id: {component_id}
      type: metadata
      status: DISABLED
      config:
        purpose: module-sdk-sample
{service_block}
  workers:
    - id: {slug}-metadata-worker
      name: {name} Metadata Worker
      kind: worker
      lifecycle: metadata
      trusted_runtime: metadata
      health_check_id: {health_id}
      required: false

  frontend_routes:
    - path: {route_path}
      name: {slug}
      component_key: sample-placeholder
      required_permission: {permission}
      enabled: false
{menu_block}{gateway_block}
  storage:
    buckets:
      - {bucket_id}

  storage_buckets:
    - id: {bucket_id}
      description: Metadata-only storage declaration for SDK compatibility tests.

  health_checks:
    - id: {health_id}
      type: metadata
      optional: true

  migrations: []

  events:
    publishes:
      - {slug}.enabled
    subscribes:
      - kernel.runtime.snapshot.generated

  scheduled_jobs: []

  admin_panels:
    - id: {slug}
      route_path: {route_path}
      required_permission: {permission}
{topology_block}"#
    )
}

fn run_runtime(command: RuntimeCommands, output: &OutputMode) -> Result<()> {
    match command {
        RuntimeCommands::Snapshot { repo_root } => {
            let snapshot = runtime_snapshot_value(&repo_root)?;
            print_value(
                &snapshot,
                output,
                "Runtime Snapshot",
                vec![
                    format!(
                        "Legacy Module 数量: {}",
                        snapshot["modules"].as_array().map(|v| v.len()).unwrap_or(0)
                    ),
                    format!(
                        "路由数量: {}",
                        snapshot["routes"].as_array().map(|v| v.len()).unwrap_or(0)
                    ),
                    format!(
                        "Service 数量: {}",
                        snapshot["services"]
                            .as_array()
                            .map(|v| v.len())
                            .unwrap_or(0)
                    ),
                ],
            )
        }
        RuntimeCommands::Routes { repo_root } => {
            let routes = runtime_routes_value(&repo_root)?;
            print_value(
                &routes,
                output,
                "Runtime Routes",
                vec![format!(
                    "路由数量: {}",
                    routes["routes"].as_array().map(|v| v.len()).unwrap_or(0)
                )],
            )
        }
        RuntimeCommands::Services { repo_root } => {
            let services = load_runtime_services(&repo_root)?;
            print_value(
                &serde_json::json!({ "services": services }),
                output,
                "Runtime Services",
                vec![format!("Service 数量: {}", services.len())],
            )
        }
        RuntimeCommands::Service {
            service_id,
            repo_root,
        } => {
            let service = find_runtime_service(&repo_root, &service_id)?;
            print_value(
                &service,
                output,
                "Runtime Service",
                vec![
                    format!("Service: {}", service.service_id),
                    format!("Legacy Module: {}", service.module_id),
                    format!("状态: {}", service.state),
                ],
            )
        }
        RuntimeCommands::PlanStart {
            service_id,
            repo_root,
            out,
        } => write_or_print_plan(
            &runtime_plan("start", &find_runtime_service(&repo_root, &service_id)?),
            out,
            output,
        ),
        RuntimeCommands::PlanStop {
            service_id,
            repo_root,
            out,
        } => write_or_print_plan(
            &runtime_plan("stop", &find_runtime_service(&repo_root, &service_id)?),
            out,
            output,
        ),
        RuntimeCommands::PlanRestart {
            service_id,
            repo_root,
            out,
        } => write_or_print_plan(
            &runtime_plan("restart", &find_runtime_service(&repo_root, &service_id)?),
            out,
            output,
        ),
        RuntimeCommands::ApplyPlan {
            plan,
            confirm,
            dry_run,
            repo_root,
            operation_log,
            verbose,
        } => apply_runtime_plan(
            &plan,
            &repo_root,
            &operation_log,
            confirm,
            dry_run,
            verbose || output.verbose,
            output,
        ),
        RuntimeCommands::Operations { operation_log } => {
            let operations = read_operation_log(&operation_log)?;
            print_value(
                &serde_json::json!({ "operations": operations }),
                output,
                "Runtime Operations",
                vec![format!("鎿嶄綔鏁伴噺: {}", operations.len())],
            )
        }
        RuntimeCommands::Operation {
            operation_id,
            operation_log,
        } => {
            let operation = read_operation_log(&operation_log)?
                .into_iter()
                .find(|item| item.operation_id == operation_id)
                .with_context(|| format!("runtime operation not found: {}", operation_id))?;
            print_value(
                &operation,
                output,
                "Runtime Operation",
                vec![
                    format!("鎿嶄綔: {}", operation.operation_id),
                    format!("鐘舵€? {}", operation.status),
                    format!("鏈嶅姟: {}", operation.service_id),
                ],
            )
        }
    }
}

#[derive(Serialize, Clone)]
struct RuntimeServiceView {
    service_id: String,
    module_id: String,
    name: String,
    kind: String,
    lifecycle: String,
    runtime: String,
    compose_service: String,
    health_check_id: String,
    routes: Vec<String>,
    required: bool,
    state: String,
    health: String,
    can_plan: bool,
    blocked_by: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RuntimePlan {
    plan_id: String,
    operation_id: String,
    module_id: String,
    service_id: String,
    action: String,
    driver: String,
    can_apply: bool,
    apply_enabled: bool,
    requires_confirmation: bool,
    dry_run: bool,
    allowed_targets: Vec<String>,
    commands: Vec<RuntimePlanCommand>,
    affected: Vec<String>,
    blocked_by: Vec<String>,
    warnings: Vec<String>,
    created_at: String,
    expires_at: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct RuntimePlanCommand {
    kind: String,
    argv: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RuntimeOperationRecord {
    operation_id: String,
    module_id: String,
    service_id: String,
    action: String,
    status: String,
    actor_username: String,
    request: Value,
    plan: RuntimePlan,
    result: Value,
    error_message: String,
    created_at: String,
    updated_at: String,
}

fn load_runtime_services(repo_root: &Path) -> Result<Vec<RuntimeServiceView>> {
    let mut out = Vec::new();
    let modules_dir = repo_root.join("modules");
    if !modules_dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(&modules_dir).context("read modules directory")? {
        let entry = entry.context("read module entry")?;
        let manifest_path = PathBuf::from("modules")
            .join(entry.file_name())
            .join("module.yaml");
        if !repo_root.join(&manifest_path).exists() {
            continue;
        }
        let manifest = validate_manifest_file(repo_root, &manifest_path)?;
        append_runtime_services(&mut out, &manifest);
    }
    out.sort_by(|left, right| {
        left.module_id
            .cmp(&right.module_id)
            .then(left.service_id.cmp(&right.service_id))
    });
    Ok(out)
}

fn append_runtime_services(out: &mut Vec<RuntimeServiceView>, manifest: &Manifest) {
    for service in &manifest.provides.services {
        out.push(runtime_service_from_service(manifest, service));
    }
    for worker in &manifest.provides.workers {
        out.push(runtime_service_from_worker(manifest, worker));
    }
}

fn runtime_service_from_service(manifest: &Manifest, service: &ServiceDecl) -> RuntimeServiceView {
    let lifecycle = default_lifecycle(&service.lifecycle);
    let runtime = default_runtime(&service.trusted_runtime, &lifecycle);
    runtime_service_view(
        manifest,
        &service.id,
        &service.name,
        default_text(&service.kind, "http"),
        lifecycle,
        runtime,
        &service.compose_service,
        &service.health_check_id,
        service.routes.clone(),
        service.required,
    )
}

fn runtime_service_from_worker(manifest: &Manifest, worker: &WorkerDecl) -> RuntimeServiceView {
    let lifecycle = default_lifecycle(&worker.lifecycle);
    let runtime = default_runtime(&worker.trusted_runtime, &lifecycle);
    runtime_service_view(
        manifest,
        &worker.id,
        &worker.name,
        default_text(&worker.kind, "worker"),
        lifecycle,
        runtime,
        &worker.compose_service,
        &worker.health_check_id,
        Vec::new(),
        worker.required,
    )
}

#[allow(clippy::too_many_arguments)]
fn runtime_service_view(
    manifest: &Manifest,
    service_id: &str,
    name: &str,
    kind: String,
    lifecycle: String,
    runtime: String,
    compose_service: &str,
    health_check_id: &str,
    routes: Vec<String>,
    required: bool,
) -> RuntimeServiceView {
    let mut blocked_by = Vec::new();
    if lifecycle == "metadata" {
        blocked_by.push("metadata lifecycle cannot start/stop".to_string());
    }
    if runtime != "compose" && lifecycle != "metadata" {
        blocked_by.push(format!("unsupported runtime {}", runtime));
    }
    if runtime == "compose" && compose_service.trim().is_empty() {
        blocked_by.push("compose_service is required".to_string());
    }
    RuntimeServiceView {
        service_id: service_id.to_string(),
        module_id: manifest.id.clone(),
        name: default_text(name, service_id),
        kind,
        lifecycle,
        runtime,
        compose_service: compose_service.trim().to_string(),
        health_check_id: health_check_id.trim().to_string(),
        routes,
        required,
        state: "DECLARED".to_string(),
        health: "unknown".to_string(),
        can_plan: blocked_by.is_empty(),
        blocked_by,
        warnings: vec!["ojosctl runtime uses controlled local operator semantics".to_string()],
    }
}

fn find_runtime_service(repo_root: &Path, service_id: &str) -> Result<RuntimeServiceView> {
    let service_id = service_id.trim();
    load_runtime_services(repo_root)?
        .into_iter()
        .find(|service| service.service_id == service_id)
        .with_context(|| format!("runtime service not found: {}", service_id))
}

fn runtime_plan(action: &str, service: &RuntimeServiceView) -> RuntimePlan {
    let now = Utc::now();
    let mut blocked_by = service.blocked_by.clone();
    if service.lifecycle == "metadata" {
        blocked_by.push(format!("metadata lifecycle cannot {}", action));
    }
    if !valid_action(action) {
        blocked_by.push(format!("unsupported action {}", action));
    }
    if service.runtime != "compose" {
        blocked_by.push(format!("unsupported runtime {}", service.runtime));
    }
    if service.compose_service.trim().is_empty() {
        blocked_by.push("compose_service is required".to_string());
    }
    let allowlist = trusted_compose_services();
    if !allowlist.contains(service.compose_service.trim()) {
        blocked_by.push("service is not in trusted compose allowlist".to_string());
    }

    let mut warnings =
        vec!["Gateway/Web apply disabled; use ojosctl/operator controlled apply".to_string()];
    let command_action = if action == "reload" {
        warnings.push("compose reload uses restart fallback".to_string());
        "restart"
    } else {
        action
    };
    let commands = if blocked_by.is_empty() {
        vec![RuntimePlanCommand {
            kind: "compose".to_string(),
            argv: vec![
                "docker".to_string(),
                "compose".to_string(),
                "--env-file".to_string(),
                DEFAULT_ENV_FILE.to_string(),
                "-f".to_string(),
                DEFAULT_COMPOSE_FILE.to_string(),
                command_action.to_string(),
                service.compose_service.clone(),
            ],
        }]
    } else {
        Vec::new()
    };

    RuntimePlan {
        plan_id: format!("runtime-{}-{}", action, service.service_id),
        operation_id: format!("runtime-{}-{}", action, Uuid::new_v4()),
        module_id: service.module_id.clone(),
        service_id: service.service_id.clone(),
        action: action.to_string(),
        driver: "compose".to_string(),
        can_apply: blocked_by.is_empty(),
        apply_enabled: false,
        requires_confirmation: true,
        dry_run: false,
        allowed_targets: vec![service.compose_service.clone()],
        commands,
        affected: vec![service.service_id.clone()],
        blocked_by,
        warnings,
        created_at: now.to_rfc3339(),
        expires_at: (now + Duration::minutes(5)).to_rfc3339(),
    }
}

fn write_or_print_plan(
    plan: &RuntimePlan,
    out: Option<PathBuf>,
    output: &OutputMode,
) -> Result<()> {
    if let Some(path) = out {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", slash_path(parent)))?;
        }
        fs::write(&path, serde_json::to_string_pretty(plan)?)
            .with_context(|| format!("write {}", slash_path(&path)))?;
        print_value(
            &serde_json::json!({
                "written": true,
                "path": slash_path(&path),
                "plan_id": plan.plan_id,
                "operation_id": plan.operation_id,
                "can_apply": plan.can_apply
            }),
            output,
            "Runtime plan written",
            vec![
                format!("plan_id: {}", plan.plan_id),
                format!("operation_id: {}", plan.operation_id),
                format!("can_apply: {}", plan.can_apply),
            ],
        )
    } else {
        print_runtime_plan(plan, output)
    }
}

fn apply_runtime_plan(
    plan_path: &Path,
    repo_root: &Path,
    operation_log: &Path,
    confirm: bool,
    dry_run: bool,
    verbose: bool,
    output: &OutputMode,
) -> Result<()> {
    let plan: RuntimePlan = serde_json::from_slice(
        &fs::read(plan_path).with_context(|| format!("read {}", slash_path(plan_path)))?,
    )
    .context("parse runtime plan json")?;
    let now = Utc::now();
    let mut blocked_by = validate_plan(&plan, repo_root, now);
    if !confirm && !dry_run {
        blocked_by.push("apply requires --confirm or --dry-run".to_string());
    }
    let mut operation = new_operation(&plan, confirm, dry_run, now);
    if !blocked_by.is_empty() {
        operation.status = "BLOCKED".to_string();
        operation.error_message = blocked_by.join("; ");
        operation.result = serde_json::json!({ "blocked_by": blocked_by });
        operation.updated_at = Utc::now().to_rfc3339();
        append_operation_log(operation_log, &operation)?;
        write_db_operation(repo_root, &operation).ok();
        print_value(
            &operation,
            output,
            "Runtime apply blocked",
            vec![operation.error_message.clone()],
        )?;
        bail!("{}", operation.error_message);
    }

    operation.status = if dry_run { "SUCCEEDED" } else { "APPLYING" }.to_string();
    append_operation_log(operation_log, &operation)?;
    write_db_operation(repo_root, &operation).ok();

    if dry_run {
        operation.result = serde_json::json!({
            "dry_run": true,
            "argv": plan.commands.iter().map(|command| command.argv.clone()).collect::<Vec<_>>()
        });
        operation.updated_at = Utc::now().to_rfc3339();
        append_operation_log(operation_log, &operation)?;
        write_db_operation(repo_root, &operation).ok();
        print_value(
            &operation,
            output,
            "Runtime apply dry-run completed",
            vec![format!("operation_id: {}", operation.operation_id)],
        )?;
        return Ok(());
    }

    let lock = match acquire_runtime_lock(repo_root, &plan) {
        Ok(lock) => lock,
        Err(err) => {
            operation.status = "BLOCKED".to_string();
            operation.error_message = redact_text(&err.to_string());
            operation.result =
                serde_json::json!({ "blocked_by": [operation.error_message.clone()] });
            operation.updated_at = Utc::now().to_rfc3339();
            append_operation_log(operation_log, &operation)?;
            write_db_operation(repo_root, &operation).ok();
            print_value(
                &operation,
                output,
                "Runtime apply blocked by lock",
                vec![operation.error_message.clone()],
            )?;
            return Err(err);
        }
    };
    let result = execute_compose_plan(&plan, repo_root, verbose);
    release_runtime_lock(&lock).ok();
    match result {
        Ok(value) => {
            operation.status = "SUCCEEDED".to_string();
            operation.result = value;
            operation.error_message.clear();
            operation.updated_at = Utc::now().to_rfc3339();
            append_operation_log(operation_log, &operation)?;
            write_db_operation(repo_root, &operation).ok();
            print_value(
                &operation,
                output,
                "Runtime apply succeeded",
                vec![format!("operation_id: {}", operation.operation_id)],
            )
        }
        Err(err) => {
            operation.status = "FAILED".to_string();
            operation.error_message = redact_text(&err.to_string());
            operation.result = serde_json::json!({ "error": operation.error_message });
            operation.updated_at = Utc::now().to_rfc3339();
            append_operation_log(operation_log, &operation)?;
            write_db_operation(repo_root, &operation).ok();
            print_value(
                &operation,
                output,
                "Runtime apply failed",
                vec![operation.error_message.clone()],
            )?;
            Err(err)
        }
    }
}
#[derive(Debug)]
struct RuntimeLock {
    path: PathBuf,
}

fn acquire_runtime_lock(repo_root: &Path, plan: &RuntimePlan) -> Result<RuntimeLock> {
    let lock_dir = repo_root.join(DEFAULT_LOCK_DIR);
    fs::create_dir_all(&lock_dir).context("create runtime lock directory")?;
    let safe_service = plan
        .service_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = lock_dir.join(format!("{}.lock", safe_service));
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(value) = serde_json::from_str::<Value>(&content)
            && let Some(expires_at) = value.get("expires_at").and_then(Value::as_str)
            && let Ok(parsed) = DateTime::parse_from_rfc3339(expires_at)
            && parsed.with_timezone(&Utc) > Utc::now()
        {
            bail!(
                "runtime operation lock is held for service {}",
                plan.service_id
            );
        }
        let _ = fs::remove_file(&path);
    }
    let expires_at =
        (Utc::now() + Duration::seconds(APPLY_TIMEOUT_SECONDS as i64 + 30)).to_rfc3339();
    let content = serde_json::json!({
        "operation_id": plan.operation_id,
        "service_id": plan.service_id,
        "expires_at": expires_at
    });
    fs::write(&path, serde_json::to_string(&content)?).context("write runtime operation lock")?;
    Ok(RuntimeLock { path })
}

fn release_runtime_lock(lock: &RuntimeLock) -> Result<()> {
    if lock.path.exists() {
        fs::remove_file(&lock.path).context("release runtime operation lock")?;
    }
    Ok(())
}

fn validate_plan(plan: &RuntimePlan, repo_root: &Path, now: DateTime<Utc>) -> Vec<String> {
    let mut blocked = Vec::new();
    if !plan.can_apply {
        blocked.push("plan can_apply is false".to_string());
    }
    if plan.driver != "compose" {
        blocked.push(format!("unsupported driver {}", plan.driver));
    }
    if !trusted_compose_services().contains(plan.service_id.trim()) {
        blocked.push("service_id is not in trusted compose allowlist".to_string());
    }
    if !valid_action(&plan.action) {
        blocked.push(format!("unsupported action {}", plan.action));
    }
    if !plan.blocked_by.is_empty() {
        blocked.push(format!("plan blocked: {}", plan.blocked_by.join("; ")));
    }
    if let Ok(expires_at) = DateTime::parse_from_rfc3339(&plan.expires_at) {
        if expires_at.with_timezone(&Utc) <= now {
            blocked.push("plan expired".to_string());
        }
    } else {
        blocked.push("plan expires_at is invalid".to_string());
    }
    if plan.commands.len() != 1 {
        blocked.push("plan must contain exactly one command".to_string());
    }
    for command in &plan.commands {
        if command.kind != "compose" {
            blocked.push(format!("unsupported command kind {}", command.kind));
        }
        validate_argv(plan, command, repo_root, &mut blocked);
    }
    blocked
}

fn validate_argv(
    plan: &RuntimePlan,
    command: &RuntimePlanCommand,
    repo_root: &Path,
    blocked: &mut Vec<String>,
) {
    let argv = &command.argv;
    if argv.len() != 8 {
        blocked.push("compose command argv shape is invalid".to_string());
        return;
    }
    if argv[0] != "docker" || argv[1] != "compose" || argv[2] != "--env-file" || argv[4] != "-f" {
        blocked.push("compose command must use docker compose argv form".to_string());
    }
    if argv.iter().any(|arg| contains_shell_metachar(arg)) {
        blocked.push("argv must not contain shell metacharacters".to_string());
    }
    if argv[3] != DEFAULT_ENV_FILE {
        blocked.push("env file must be the trusted default .env".to_string());
    }
    if argv[5] != DEFAULT_COMPOSE_FILE {
        blocked.push("compose file must be trusted deploy/compose/docker-compose.yml".to_string());
    }
    let action = if plan.action == "reload" {
        "restart"
    } else {
        plan.action.as_str()
    };
    if argv[6] != action {
        blocked.push("command action does not match plan action".to_string());
    }
    let service = argv[7].trim();
    if service
        != plan
            .allowed_targets
            .first()
            .map(String::as_str)
            .unwrap_or("")
    {
        blocked.push("command target is not the plan allowed target".to_string());
    }
    if !trusted_compose_services().contains(service) {
        blocked.push("service is not in trusted compose allowlist".to_string());
    }
    if !repo_root.join(DEFAULT_COMPOSE_FILE).is_file() {
        blocked.push("trusted compose file is missing".to_string());
    }
}

fn execute_compose_plan(plan: &RuntimePlan, repo_root: &Path, verbose: bool) -> Result<Value> {
    let command = plan
        .commands
        .first()
        .context("plan has no command after validation")?;
    let mut process = Command::new(&command.argv[0]);
    process
        .args(&command.argv[1..])
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process
        .spawn()
        .with_context(|| format!("execute {}", command.argv.join(" ")))?;
    let started = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > StdDuration::from_secs(APPLY_TIMEOUT_SECONDS) {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "compose apply timed out after {} seconds",
                APPLY_TIMEOUT_SECONDS
            );
        }
        thread::sleep(StdDuration::from_millis(200));
    }
    let output = child
        .wait_with_output()
        .context("collect compose command output")?;
    let stdout = limit_output(&redact_text(&String::from_utf8_lossy(&output.stdout)));
    let stderr = limit_output(&redact_text(&String::from_utf8_lossy(&output.stderr)));
    if !output.status.success() {
        bail!(
            "compose apply failed status={}: {}",
            output.status.code().unwrap_or(-1),
            stderr
        );
    }
    Ok(serde_json::json!({
        "dry_run": false,
        "timeout_seconds": APPLY_TIMEOUT_SECONDS,
        "argv": if verbose { command.argv.clone() } else { vec!["docker".to_string(), "compose".to_string(), command.argv[6].clone(), command.argv[7].clone()] },
        "exit_code": output.status.code().unwrap_or(0),
        "stdout": stdout,
        "stderr": stderr
    }))
}

fn new_operation(
    plan: &RuntimePlan,
    confirm: bool,
    dry_run: bool,
    now: DateTime<Utc>,
) -> RuntimeOperationRecord {
    RuntimeOperationRecord {
        operation_id: plan.operation_id.clone(),
        module_id: plan.module_id.clone(),
        service_id: plan.service_id.clone(),
        action: format!("runtime.{}", plan.action),
        status: "PLANNED".to_string(),
        actor_username: whoami(),
        request: serde_json::json!({ "dry_run": dry_run, "confirm": confirm }),
        plan: plan.clone(),
        result: serde_json::json!({}),
        error_message: String::new(),
        created_at: now.to_rfc3339(),
        updated_at: now.to_rfc3339(),
    }
}

fn append_operation_log(path: &Path, operation: &RuntimeOperationRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", slash_path(parent)))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", slash_path(path)))?;
    writeln!(file, "{}", serde_json::to_string(operation)?).context("write operation log")
}

fn read_operation_log(path: &Path) -> Result<Vec<RuntimeOperationRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).with_context(|| format!("read {}", slash_path(path)))?;
    let mut latest = BTreeMap::<String, RuntimeOperationRecord>::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        if let Ok(item) = serde_json::from_str::<RuntimeOperationRecord>(line) {
            latest.insert(item.operation_id.clone(), item);
        }
    }
    let mut items = latest.into_values().collect::<Vec<_>>();
    items.sort_by(|left, right| left.updated_at.cmp(&right.updated_at));
    items.reverse();
    Ok(items)
}

fn write_db_operation(repo_root: &Path, operation: &RuntimeOperationRecord) -> Result<()> {
    let audit_sql = if matches!(
        operation.status.as_str(),
        "SUCCEEDED" | "FAILED" | "BLOCKED" | "EXPIRED"
    ) {
        format!(
            r#"
INSERT INTO permission_audit_logs(actor_type, actor_id, action, target_type, target_id, metadata)
VALUES ('operator', 0, '{action}', 'runtime_service', 0, '{{"operation_id":"{op}","service_id":"{service}"}}'::jsonb);
"#,
            action = sql_escape(&operation.action),
            op = sql_escape(&operation.operation_id),
            service = sql_escape(&operation.service_id),
        )
    } else {
        String::new()
    };
    let sql = format!(
        r#"
INSERT INTO module_operations(operation_id, module_id, action, status, actor_username, request, plan, result, error_message)
VALUES ('{op}', '{module}', '{action}', '{status}', '{actor}', '{request}'::jsonb, '{plan}'::jsonb, '{result}'::jsonb, '{error}')
ON CONFLICT(operation_id) DO UPDATE SET
    status = EXCLUDED.status,
    result = EXCLUDED.result,
    error_message = EXCLUDED.error_message,
    updated_at = NOW();
{audit_sql}
"#,
        op = sql_escape(&operation.operation_id),
        module = sql_escape(&operation.module_id),
        action = sql_escape(&operation.action),
        status = sql_escape(&operation.status),
        actor = sql_escape(&operation.actor_username),
        request = sql_escape(&serde_json::to_string(&operation.request)?),
        plan = sql_escape(&serde_json::to_string(&operation.plan)?),
        result = sql_escape(&serde_json::to_string(&operation.result)?),
        error = sql_escape(&operation.error_message),
        audit_sql = audit_sql,
    );
    let output = Command::new("docker")
        .args([
            "compose",
            "--env-file",
            DEFAULT_ENV_FILE,
            "-f",
            DEFAULT_COMPOSE_FILE,
            "exec",
            "-T",
            "postgres",
            "psql",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "ojos",
        ])
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(sql.as_bytes())?;
            }
            child.wait_with_output()
        })
        .context("write runtime operation to control-plane database")?;
    if !output.status.success() {
        bail!("database operation write failed");
    }
    Ok(())
}

fn trusted_compose_services() -> BTreeSet<&'static str> {
    [
        "auth",
        "gateway",
        "root-runtime-manager",
        "problem-api",
        "judge-api",
        "judge-worker",
    ]
    .into_iter()
    .collect()
}

fn valid_action(action: &str) -> bool {
    matches!(action, "start" | "stop" | "restart" | "reload")
}

fn contains_shell_metachar(value: &str) -> bool {
    value.contains(';') || value.contains('&') || value.contains('|') || value.contains('`')
}

fn default_lifecycle(value: &str) -> String {
    default_text(value, "managed").to_ascii_lowercase()
}

fn default_runtime(value: &str, lifecycle: &str) -> String {
    if !value.trim().is_empty() {
        return value.trim().to_ascii_lowercase();
    }
    if lifecycle == "metadata" {
        "metadata".to_string()
    } else {
        "compose".to_string()
    }
}

fn default_text(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "operator".to_string())
}

fn sql_escape(value: &str) -> String {
    redact_text(value).replace('\'', "''")
}

fn redact_text(value: &str) -> String {
    let mut out = value.to_string();
    for key in ["token", "secret", "password", "authorization"] {
        out = replace_case_insensitive(&out, key, "[redacted]");
    }
    out
}

fn replace_case_insensitive(value: &str, needle: &str, replacement: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut result = String::new();
    let mut cursor = 0usize;
    let mut search = 0usize;
    while let Some(pos) = lower[search..].find(&needle_lower) {
        let start = search + pos;
        result.push_str(&value[cursor..start]);
        result.push_str(replacement);
        cursor = start + needle.len();
        search = cursor;
    }
    result.push_str(&value[cursor..]);
    result
}

fn limit_output(value: &str) -> String {
    if value.len() > OUTPUT_LIMIT {
        format!("{}...[truncated]", &value[..OUTPUT_LIMIT])
    } else {
        value.to_string()
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_value(
    value: &impl Serialize,
    output: &OutputMode,
    title: &str,
    lines: Vec<String>,
) -> Result<()> {
    if output.json {
        return print_json(value);
    }
    println!("{}", title);
    for line in lines {
        println!("  - {}", redact_text(&line));
    }
    Ok(())
}

fn print_plan(plan: &Plan, output: &OutputMode) -> Result<()> {
    print_value(
        plan,
        output,
        "legacy module plan",
        vec![
            format!("kind: {:?}", plan.kind),
            format!("legacy module: {}", plan.module_id),
            format!("version: {}", plan.version),
            format!("can_apply: {}", plan.can_apply),
            format!(
                "blocked_by: {}",
                if plan.blocked_by.is_empty() {
                    "none".to_string()
                } else {
                    plan.blocked_by.join("; ")
                }
            ),
            format!("actions: {}", plan.actions.len()),
        ],
    )
}

fn print_runtime_plan(plan: &RuntimePlan, output: &OutputMode) -> Result<()> {
    print_value(
        plan,
        output,
        "Runtime plan",
        vec![
            format!("plan_id: {}", plan.plan_id),
            format!("operation_id: {}", plan.operation_id),
            format!("service: {}", plan.service_id),
            format!("action: {}", plan.action),
            format!("can_apply: {}", plan.can_apply),
            format!("requires_confirmation: {}", plan.requires_confirmation),
            format!("expires_at: {}", plan.expires_at),
        ],
    )
}

fn ok_text(ok: bool) -> &'static str {
    if ok { "ok" } else { "missing" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn service() -> RuntimeServiceView {
        RuntimeServiceView {
            service_id: "problem-api".to_string(),
            module_id: "ojos.judge-core".to_string(),
            name: "Problem API".to_string(),
            kind: "http".to_string(),
            lifecycle: "managed".to_string(),
            runtime: "compose".to_string(),
            compose_service: "problem-api".to_string(),
            health_check_id: "problem-api-health".to_string(),
            routes: vec!["/api/problem".to_string()],
            required: true,
            state: "DECLARED".to_string(),
            health: "unknown".to_string(),
            can_plan: true,
            blocked_by: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn plan_json_uses_argv_and_ttl() {
        let plan = runtime_plan("restart", &service());
        assert!(plan.can_apply);
        assert!(plan.requires_confirmation);
        assert_eq!(plan.commands[0].kind, "compose");
        assert_eq!(plan.commands[0].argv[0], "docker");
        assert!(plan.commands[0].argv.contains(&"restart".to_string()));
        assert!(DateTime::parse_from_rfc3339(&plan.expires_at).is_ok());
    }

    #[test]
    fn validate_rejects_expired_plan() {
        let mut plan = runtime_plan("restart", &service());
        plan.expires_at = (Utc::now() - Duration::minutes(1)).to_rfc3339();
        let blocked = validate_plan(&plan, Path::new("."), Utc::now());
        assert!(blocked.iter().any(|item| item == "plan expired"));
    }

    #[test]
    fn validate_rejects_bad_service_and_shell_meta() {
        let mut plan = runtime_plan("restart", &service());
        plan.service_id = "not-allowed".to_string();
        plan.allowed_targets = vec!["not-allowed".to_string()];
        plan.commands[0].argv[7] = "problem-api;bad".to_string();
        let blocked = validate_plan(&plan, Path::new("."), Utc::now());
        assert!(blocked.iter().any(|item| item.contains("shell")));
        assert!(
            blocked
                .iter()
                .any(|item| item.contains("trusted compose allowlist"))
        );
    }

    #[test]
    fn validate_rejects_bad_action() {
        let mut plan = runtime_plan("restart", &service());
        plan.action = "exec".to_string();
        let blocked = validate_plan(&plan, Path::new("."), Utc::now());
        assert!(
            blocked
                .iter()
                .any(|item| item.contains("unsupported action"))
        );
    }

    #[test]
    fn metadata_lifecycle_blocks_apply() {
        let mut metadata = service();
        metadata.service_id = "demo-metadata-service".to_string();
        metadata.lifecycle = "metadata".to_string();
        metadata.runtime = "metadata".to_string();
        metadata.compose_service.clear();
        let plan = runtime_plan("start", &metadata);
        assert!(!plan.can_apply);
        assert!(
            plan.blocked_by
                .iter()
                .any(|item| item.contains("metadata lifecycle cannot start"))
        );
    }

    #[test]
    fn redact_text_removes_sensitive_words() {
        let redacted = redact_text("Authorization token secret password");
        assert!(!redacted.to_ascii_lowercase().contains("authorization"));
        assert!(!redacted.to_ascii_lowercase().contains("token"));
        assert!(!redacted.to_ascii_lowercase().contains("secret"));
        assert!(!redacted.to_ascii_lowercase().contains("password"));
    }

    #[test]
    fn runtime_lock_blocks_concurrent_apply() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("deploy/compose")).expect("compose dir");
        fs::write(dir.path().join(DEFAULT_COMPOSE_FILE), "services: {}\n").expect("compose file");
        let plan = runtime_plan("restart", &service());
        let lock = acquire_runtime_lock(dir.path(), &plan).expect("first lock");
        let second = acquire_runtime_lock(dir.path(), &plan).expect_err("second lock should fail");
        assert!(
            second
                .to_string()
                .contains("runtime operation lock is held")
        );
        release_runtime_lock(&lock).expect("release");
        acquire_runtime_lock(dir.path(), &plan).expect("reacquire after release");
    }

    #[test]
    fn module_init_scaffold_generates_valid_metadata_module() {
        let dir = tempdir().expect("tempdir");
        let out = dir.path().join("sample-hello");
        scaffold_module(
            ModuleScaffoldOptions {
                module_id: "ojos.sample-hello".to_string(),
                name: "Sample Hello".to_string(),
                kind: "feature".to_string(),
                out: out.clone(),
                force: false,
                with_service: "metadata".to_string(),
                with_gateway_route: "disabled".to_string(),
                with_menu: "disabled".to_string(),
                with_topology: true,
            },
            &OutputMode {
                json: true,
                verbose: false,
            },
        )
        .expect("scaffold");

        for rel in [
            "module.yaml",
            "README.md",
            "migrations",
            "frontend/contributions.yaml",
            "services/README.md",
            "tests/module-smoke.md",
        ] {
            assert!(out.join(rel).exists(), "{rel} should exist");
        }
        let text = fs::read_to_string(out.join("module.yaml")).expect("manifest");
        assert!(!text.contains("command:"));
        assert!(!text.contains("script:"));
        assert!(!text.contains("hook:"));
        assert!(!text.contains("target_url:"));
        let manifest: Manifest = serde_yaml::from_str(&text).expect("parse manifest");
        validate_manifest(&manifest).expect("validate generated manifest");
        assert_eq!(manifest.id, "ojos.sample-hello");
        assert_eq!(manifest.provides.services[0].lifecycle, "metadata");
        assert!(!manifest.provides.gateway_routes[0].enabled);
    }

    #[test]
    fn module_init_refuses_existing_directory_without_force() {
        let dir = tempdir().expect("tempdir");
        let out = dir.path().join("sample-hello");
        fs::create_dir_all(&out).expect("out");
        let err = scaffold_module(
            ModuleScaffoldOptions {
                module_id: "ojos.sample-hello".to_string(),
                name: "Sample Hello".to_string(),
                kind: "feature".to_string(),
                out,
                force: false,
                with_service: "metadata".to_string(),
                with_gateway_route: "disabled".to_string(),
                with_menu: "disabled".to_string(),
                with_topology: false,
            },
            &OutputMode {
                json: true,
                verbose: false,
            },
        )
        .expect_err("existing directory should fail");
        assert!(err.to_string().contains("already exists"));
    }
}
