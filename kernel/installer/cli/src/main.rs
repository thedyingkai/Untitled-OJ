use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use service_installer_core::{
    ServiceManifest, expand_set, package_service, service_install_plan, validate_endpoint_id,
    validate_service_manifest_file, validate_service_set_file, verify_package,
};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration as StdDuration, Instant};
use uuid::Uuid;

const DEFAULT_COMPOSE_FILE: &str = "deploy/compose/docker-compose.yml";
const DEFAULT_ENV_FILE: &str = ".env";
const EXAMPLE_ENV_FILE: &str = ".env.example";
const DEFAULT_OPERATION_LOG: &str = ".tmp/agent/runtime-operations.jsonl";
const DEFAULT_LOCK_DIR: &str = ".tmp/agent/runtime-locks";
const APPLY_TIMEOUT_SECONDS: u64 = 60;
const DEFAULT_LOCK_TTL_SECONDS: i64 = 300;
const OUTPUT_LIMIT: usize = 4096;

#[derive(Parser)]
#[command(name = "ojosctl")]
#[command(about = "OJOS Root Runtime Manager CLI")]
#[command(version)]
struct Cli {
    #[arg(long, global = true, help = "以 JSON 输出，供脚本和 CI 使用")]
    json: bool,
    #[arg(long, global = true, help = "显示受控执行细节")]
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
    Device {
        #[command(subcommand)]
        command: DeviceCommands,
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
    Install {
        manifest: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        #[arg(long)]
        dry_run: bool,
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

#[derive(Subcommand)]
enum DeviceCommands {
    List,
    ValidateNonRootPlan {
        set: PathBuf,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
}

#[derive(Copy, Clone)]
struct OutputMode {
    json: bool,
    verbose: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeServiceView {
    service_id: String,
    name: String,
    kind: String,
    lifecycle: String,
    runtime: String,
    compose_service: String,
    endpoint: String,
    routes: Vec<String>,
    required: bool,
    state: String,
    health: String,
    can_plan: bool,
    blocked_by: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimePlan {
    plan_id: String,
    operation_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimePlanCommand {
    kind: String,
    argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeOperationRecord {
    operation_id: String,
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

#[derive(Debug)]
struct RuntimeLock {
    path: PathBuf,
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
        Commands::Service { command } => run_service(command, &output),
        Commands::Set { command } => run_set(command, &output),
        Commands::Endpoint { command } => run_endpoint(command, &output),
        Commands::Link { command } => run_link(command, &output),
        Commands::Topology { command } => run_topology(command, &output),
        Commands::Runtime { command } => run_runtime(command, &output),
        Commands::Device { command } => run_device(command, &output),
    }
}

fn run_service(command: ServiceCommands, output: &OutputMode) -> Result<()> {
    match command {
        ServiceCommands::Discover { repo_root } => {
            let services = discover_service_manifests(&repo_root)?;
            print_value(
                &serde_json::json!({ "services": services }),
                output,
                "Service discovery completed",
                vec![format!("services: {}", services.len())],
            )
        }
        ServiceCommands::Validate {
            manifest,
            repo_root,
        } => {
            let manifest = validate_service_manifest_file(&repo_root, &manifest)?;
            print_value(
                &serde_json::json!({ "valid": true, "service": manifest }),
                output,
                "Service contract valid",
                vec![
                    format!("Service: {}", manifest.id),
                    format!("version: {}", manifest.version),
                    format!("default_port: {}", manifest.endpoint.default_port),
                ],
            )
        }
        ServiceCommands::InstallPlan {
            manifest,
            repo_root,
        }
        | ServiceCommands::Install {
            manifest,
            repo_root,
            dry_run: _,
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
        ServiceCommands::Enable { service_id } => service_state_plan(&service_id, "enable", output),
        ServiceCommands::Disable { service_id } => {
            service_state_plan(&service_id, "disable", output)
        }
    }
}

fn service_state_plan(service_id: &str, action: &str, output: &OutputMode) -> Result<()> {
    print_value(
        &serde_json::json!({
            "service_id": service_id,
            "action": action,
            "status": "planned",
            "note": "Root Runtime Manager applies Service state operations"
        }),
        output,
        "Service state plan",
        vec![format!("{} {}", action, service_id)],
    )
}

fn run_set(command: SetCommands, output: &OutputMode) -> Result<()> {
    match command {
        SetCommands::List { repo_root } => {
            let sets = discover_sets(&repo_root)?;
            print_value(
                &serde_json::json!({ "sets": sets }),
                output,
                "Set list",
                vec![format!("sets: {}", sets.len())],
            )
        }
        SetCommands::Validate { set, repo_root } => {
            let set = validate_service_set_file(&repo_root, &set)?;
            print_value(
                &serde_json::json!({ "valid": true, "set": set }),
                output,
                "Set valid",
                vec![format!("Set: {}", set.id)],
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
                &serde_json::json!({ "valid": true, "endpoint": endpoint }),
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
                    "service_id": service_id,
                    "endpoint": endpoint,
                    "primary_id": endpoint
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
                    "source": source,
                    "target": target,
                    "primary_id": format!("{} -> {}", source, target),
                    "protocol": protocol,
                    "auth_mode": auth_mode
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
            let value = topology_snapshot(&repo_root)?;
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

fn run_runtime(command: RuntimeCommands, output: &OutputMode) -> Result<()> {
    match command {
        RuntimeCommands::Snapshot { repo_root } => {
            let services = runtime_services(&repo_root)?;
            let value = serde_json::json!({
                "version": "service-runtime-v1",
                "generated_at": Utc::now().to_rfc3339(),
                "services": services,
                "topology": topology_snapshot(&repo_root)?,
                "warnings": []
            });
            print_value(
                &value,
                output,
                "Runtime snapshot",
                vec![format!(
                    "services: {}",
                    value["services"].as_array().map(|v| v.len()).unwrap_or(0)
                )],
            )
        }
        RuntimeCommands::Routes { repo_root } => {
            let routes = runtime_routes(&repo_root)?;
            let value = serde_json::json!({
                "version": "service-runtime-v1",
                "generated_at": Utc::now().to_rfc3339(),
                "routes": routes,
                "warnings": [],
                "can_proxy": true
            });
            print_value(
                &value,
                output,
                "Runtime routes",
                vec![format!(
                    "routes: {}",
                    value["routes"].as_array().map(|v| v.len()).unwrap_or(0)
                )],
            )
        }
        RuntimeCommands::Services { repo_root } => {
            let services = runtime_services(&repo_root)?;
            let workers = services
                .iter()
                .filter(|item| item.kind.contains("worker"))
                .cloned()
                .collect::<Vec<_>>();
            let backends = services
                .into_iter()
                .filter(|item| !item.kind.contains("worker"))
                .collect::<Vec<_>>();
            print_value(
                &serde_json::json!({ "services": backends, "workers": workers }),
                output,
                "Runtime services",
                vec![],
            )
        }
        RuntimeCommands::Service {
            service_id,
            repo_root,
        } => {
            let service = get_runtime_service(&repo_root, &service_id)?;
            print_value(
                &serde_json::json!({ "service": service }),
                output,
                "Runtime service",
                vec![format!("Service: {}", service_id)],
            )
        }
        RuntimeCommands::PlanStart {
            service_id,
            repo_root,
            out,
        } => plan_runtime_action(&repo_root, &service_id, "start", out, output),
        RuntimeCommands::PlanStop {
            service_id,
            repo_root,
            out,
        } => plan_runtime_action(&repo_root, &service_id, "stop", out, output),
        RuntimeCommands::PlanRestart {
            service_id,
            repo_root,
            out,
        } => plan_runtime_action(&repo_root, &service_id, "restart", out, output),
        RuntimeCommands::ApplyPlan {
            plan,
            confirm,
            dry_run,
            repo_root,
            operation_log,
            verbose,
        } => apply_runtime_plan(
            &repo_root,
            &plan,
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
                "Runtime operations",
                vec![format!("operations: {}", operations.len())],
            )
        }
        RuntimeCommands::Operation {
            operation_id,
            operation_log,
        } => {
            let operations = read_operation_log(&operation_log)?;
            let Some(operation) = operations
                .into_iter()
                .find(|item| item.operation_id == operation_id)
            else {
                bail!("runtime operation not found");
            };
            print_value(&operation, output, "Runtime operation", vec![operation_id])
        }
    }
}

fn run_device(command: DeviceCommands, output: &OutputMode) -> Result<()> {
    match command {
        DeviceCommands::List => print_value(
            &serde_json::json!({
                "devices": [
                    { "device_id": "root-local", "kind": "root", "endpoint": "127.0.0.1:0", "health": "unknown" }
                ]
            }),
            output,
            "Device list",
            vec!["root-local".to_string()],
        ),
        DeviceCommands::ValidateNonRootPlan { set, repo_root } => {
            let set = validate_service_set_file(&repo_root, &set)?;
            let blocked = if set.services.iter().any(|item| item == "web-shell") {
                vec!["non-root set cannot include web-shell".to_string()]
            } else {
                vec![]
            };
            print_value(
                &serde_json::json!({
                    "set_id": set.id,
                    "non_root_only": set.non_root_only,
                    "valid": blocked.is_empty(),
                    "blocked_by": blocked
                }),
                output,
                "Non-root device plan",
                vec![format!("valid: {}", blocked.is_empty())],
            )
        }
    }
}

fn run_doctor(repo_root: &Path, output: &OutputMode) -> Result<()> {
    let services_dir = repo_root.join("services");
    let sets_dir = repo_root.join("sets");
    let compose_file = repo_root.join(DEFAULT_COMPOSE_FILE);
    let services = discover_service_manifests(repo_root)?;
    let sets = discover_sets(repo_root)?;
    let value = serde_json::json!({
        "ok": services_dir.is_dir() && sets_dir.is_dir() && compose_file.is_file() && !services.is_empty() && !sets.is_empty(),
        "services_dir_exists": services_dir.is_dir(),
        "sets_dir_exists": sets_dir.is_dir(),
        "compose_file_exists": compose_file.is_file(),
        "service_count": services.len(),
        "set_count": sets.len(),
        "package_format": ".ojossvc"
    });
    print_value(
        &value,
        output,
        "OJOS service-first doctor",
        vec![
            format!("services 目录: {}", ok_text(services_dir.is_dir())),
            format!("sets 目录: {}", ok_text(sets_dir.is_dir())),
            format!("compose 文件: {}", ok_text(compose_file.is_file())),
        ],
    )?;
    if value["ok"].as_bool() == Some(false) {
        bail!("service-first workspace is incomplete");
    }
    Ok(())
}

fn run_status(repo_root: &Path, operation_log: &Path, output: &OutputMode) -> Result<()> {
    let services = discover_service_manifests(repo_root)?;
    let sets = discover_sets(repo_root)?;
    let operations = read_operation_log(operation_log)?;
    print_value(
        &serde_json::json!({
            "status": "service-first",
            "services": services.len(),
            "sets": sets.len(),
            "operations": operations.len(),
        }),
        output,
        "OJOS status",
        vec![
            format!("services: {}", services.len()),
            format!("sets: {}", sets.len()),
            format!("operations: {}", operations.len()),
        ],
    )
}

fn discover_service_manifests(repo_root: &Path) -> Result<Vec<Value>> {
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
                    Ok(manifest) => items.push(service_discovery_item(&manifest, &manifest_path)),
                    Err(err) => items.push(serde_json::json!({
                        "manifest_path": slash_path(&manifest_path),
                        "valid": false,
                        "error": err.to_string()
                    })),
                }
            }
        }
    }
    items.sort_by_key(|item| item["service_id"].as_str().unwrap_or_default().to_string());
    Ok(items)
}

fn service_discovery_item(manifest: &ServiceManifest, manifest_path: &Path) -> Value {
    serde_json::json!({
        "manifest_path": slash_path(manifest_path),
        "service_id": manifest.id,
        "name": manifest.name,
        "version": manifest.version,
        "kind": manifest.kind,
        "default_endpoint": format!("0.0.0.0:{}", manifest.endpoint.default_port),
        "valid": true
    })
}

fn discover_sets(repo_root: &Path) -> Result<Vec<Value>> {
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
    items.sort_by_key(|item| item["set_id"].as_str().unwrap_or_default().to_string());
    Ok(items)
}

fn runtime_services(repo_root: &Path) -> Result<Vec<RuntimeServiceView>> {
    let mut out = Vec::new();
    for item in discover_service_manifests(repo_root)? {
        if item["valid"].as_bool() != Some(true) {
            continue;
        }
        let service_id = item["service_id"].as_str().unwrap_or_default().to_string();
        let manifest_path = item["manifest_path"].as_str().unwrap_or_default();
        let manifest = validate_service_manifest_file(repo_root, Path::new(manifest_path))?;
        out.push(RuntimeServiceView {
            service_id: service_id.clone(),
            name: manifest.name,
            kind: manifest.kind,
            lifecycle: "managed".to_string(),
            runtime: format!("{:?}", manifest.runtime.mode).to_ascii_lowercase(),
            compose_service: if trusted_compose_services().contains(service_id.as_str()) {
                service_id.clone()
            } else {
                String::new()
            },
            endpoint: format!("0.0.0.0:{}", manifest.endpoint.default_port),
            routes: inferred_routes(&service_id),
            required: true,
            state: "DECLARED".to_string(),
            health: "unknown".to_string(),
            can_plan: trusted_compose_services().contains(service_id.as_str()),
            blocked_by: vec![],
            warnings: vec![],
        });
    }
    out.sort_by(|a, b| a.service_id.cmp(&b.service_id));
    Ok(out)
}

fn inferred_routes(service_id: &str) -> Vec<String> {
    match service_id {
        "auth" => vec!["/api/auth".to_string()],
        "gateway" => vec!["/api".to_string()],
        "problem-api" => vec!["/api/problem".to_string()],
        "judge-api" => vec!["/api/judge".to_string()],
        "web-shell" => vec!["/".to_string()],
        _ => vec![],
    }
}

fn runtime_routes(repo_root: &Path) -> Result<Vec<Value>> {
    let services = runtime_services(repo_root)?;
    let mut routes = Vec::new();
    for service in services {
        for route in &service.routes {
            routes.push(serde_json::json!({
                "route_id": format!("{}:{}", service.service_id, route),
                "prefix": route,
                "service_id": service.service_id,
                "target_service": service.service_id,
                "auth_mode": if route.starts_with("/api/admin") { "admin" } else { "user" },
                "enabled": true,
                "proxy_enabled": service.can_plan,
                "status": service.state
            }));
        }
    }
    Ok(routes)
}

fn get_runtime_service(repo_root: &Path, service_id: &str) -> Result<RuntimeServiceView> {
    runtime_services(repo_root)?
        .into_iter()
        .find(|service| service.service_id == service_id)
        .with_context(|| format!("service {} is not declared", service_id))
}

fn topology_snapshot(repo_root: &Path) -> Result<Value> {
    let services = discover_service_manifests(repo_root)?;
    let sets = discover_sets(repo_root)?;
    let endpoints = services
        .iter()
        .filter_map(|item| item.get("default_endpoint").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut links = Vec::new();
    for set in &sets {
        if let Some(default_links) = set.get("default_links").and_then(|v| v.as_array()) {
            for link in default_links {
                links.push(link.clone());
            }
        }
    }
    Ok(serde_json::json!({
        "devices": [{ "device_id": "root-local", "kind": "root", "health": "unknown" }],
        "services": services,
        "sets": sets,
        "endpoints": endpoints,
        "links": links,
        "views": ["set", "service", "endpoint", "link", "device", "health"]
    }))
}

fn plan_runtime_action(
    repo_root: &Path,
    service_id: &str,
    action: &str,
    out: Option<PathBuf>,
    output: &OutputMode,
) -> Result<()> {
    let service = get_runtime_service(repo_root, service_id)?;
    let plan = runtime_plan(repo_root, action, &service);
    if let Some(path) = out {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(&plan)?)?;
    }
    print_runtime_plan(&plan, output)
}

fn runtime_plan(repo_root: &Path, action: &str, service: &RuntimeServiceView) -> RuntimePlan {
    let now = Utc::now();
    let mut blocked_by = Vec::new();
    if !service.can_plan || service.compose_service.trim().is_empty() {
        blocked_by.push("service is not in trusted compose allowlist".to_string());
    }
    if !matches!(action, "start" | "stop" | "restart" | "reload") {
        blocked_by.push(format!("unsupported action {}", action));
    }
    let compose_action = if action == "reload" {
        "restart"
    } else {
        action
    };
    RuntimePlan {
        plan_id: Uuid::new_v4().to_string(),
        operation_id: Uuid::new_v4().to_string(),
        service_id: service.service_id.clone(),
        action: action.to_string(),
        driver: "docker-compose".to_string(),
        can_apply: blocked_by.is_empty(),
        apply_enabled: true,
        requires_confirmation: true,
        dry_run: true,
        allowed_targets: vec![service.compose_service.clone()],
        commands: vec![RuntimePlanCommand {
            kind: "compose".to_string(),
            argv: vec![
                "docker".to_string(),
                "compose".to_string(),
                "--env-file".to_string(),
                trusted_env_file(repo_root).to_string(),
                "-f".to_string(),
                DEFAULT_COMPOSE_FILE.to_string(),
                compose_action.to_string(),
                service.compose_service.clone(),
            ],
        }],
        affected: vec![service.service_id.clone()],
        blocked_by,
        warnings: vec![],
        created_at: now.to_rfc3339(),
        expires_at: (now + Duration::minutes(5)).to_rfc3339(),
    }
}

fn trusted_env_file(repo_root: &Path) -> &'static str {
    if repo_root.join(DEFAULT_ENV_FILE).is_file() {
        DEFAULT_ENV_FILE
    } else {
        EXAMPLE_ENV_FILE
    }
}

fn apply_runtime_plan(
    repo_root: &Path,
    plan_path: &Path,
    operation_log: &Path,
    confirm: bool,
    dry_run: bool,
    verbose: bool,
    output: &OutputMode,
) -> Result<()> {
    let text =
        fs::read_to_string(plan_path).with_context(|| format!("read {}", slash_path(plan_path)))?;
    let mut plan: RuntimePlan = serde_json::from_str(&text)?;
    let mut operation = new_operation(&plan, confirm, dry_run, Utc::now());
    let blocked = validate_plan(&plan, repo_root, Utc::now());
    if !blocked.is_empty() {
        plan.can_apply = false;
        plan.blocked_by = blocked;
        operation.plan = plan.clone();
        operation.status = "BLOCKED".to_string();
        operation.error_message = plan.blocked_by.join("; ");
        append_operation_log(operation_log, &operation)?;
        let _ = write_db_operation(repo_root, &operation);
        print_value(
            &operation,
            output,
            "Runtime apply blocked",
            vec![operation.error_message.clone()],
        )?;
        bail!("runtime plan blocked");
    }
    if !confirm && !dry_run {
        bail!("runtime apply requires --confirm or --dry-run");
    }
    let lock = acquire_runtime_lock(repo_root, &plan)?;
    let result = if dry_run {
        Ok(serde_json::json!({ "dry_run": true, "argv": plan.commands[0].argv }))
    } else {
        execute_compose_plan(&plan, repo_root, verbose)
    };
    release_runtime_lock(&lock)?;
    let result = result?;
    operation.status = "SUCCEEDED".to_string();
    operation.result = result;
    operation.updated_at = Utc::now().to_rfc3339();
    append_operation_log(operation_log, &operation)?;
    let _ = write_db_operation(repo_root, &operation);
    print_value(
        &operation,
        output,
        "Runtime apply completed",
        vec![format!("operation: {}", operation.operation_id)],
    )
}

fn validate_plan(plan: &RuntimePlan, repo_root: &Path, now: DateTime<Utc>) -> Vec<String> {
    let mut blocked = Vec::new();
    if let Ok(expires) = DateTime::parse_from_rfc3339(&plan.expires_at) {
        if expires.with_timezone(&Utc) < now {
            blocked.push("plan expired".to_string());
        }
    } else {
        blocked.push("plan expires_at is invalid".to_string());
    }
    if !valid_action(&plan.action) {
        blocked.push(format!("unsupported action {}", plan.action));
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
    let trusted_env = trusted_env_file(repo_root);
    if argv[3] != trusted_env {
        blocked.push(format!("env file must be trusted {}", trusted_env));
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
    let command = plan.commands.first().context("plan has no command")?;
    let mut process = Command::new(&command.argv[0]);
    process
        .args(&command.argv[1..])
        .current_dir(repo_root)
        .envs(static_compose_env(repo_root))
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

fn acquire_runtime_lock(repo_root: &Path, plan: &RuntimePlan) -> Result<RuntimeLock> {
    let lock_dir = repo_root.join(DEFAULT_LOCK_DIR);
    fs::create_dir_all(&lock_dir)?;
    let path = lock_dir.join(format!("{}.lock", plan.service_id));
    clear_expired_runtime_lock(&path)?;
    let file = OpenOptions::new().write(true).create_new(true).open(&path);
    match file {
        Ok(mut file) => {
            writeln!(file, "{} {}", plan.operation_id, Utc::now().to_rfc3339())?;
            Ok(RuntimeLock { path })
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("runtime operation lock is held for {}", plan.service_id)
        }
        Err(err) => Err(err.into()),
    }
}

fn clear_expired_runtime_lock(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(path).unwrap_or_default();
    let locked_at = content
        .split_whitespace()
        .nth(1)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let ttl = env::var("ROOT_RUNTIME_MANAGER_LOCK_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| (30..=3600).contains(value))
        .unwrap_or(DEFAULT_LOCK_TTL_SECONDS);
    if let Some(locked_at) = locked_at {
        if Utc::now() - locked_at < Duration::seconds(ttl) {
            return Ok(());
        }
    }
    fs::remove_file(path).with_context(|| format!("remove expired lock {}", slash_path(path)))
}

fn release_runtime_lock(lock: &RuntimeLock) -> Result<()> {
    if lock.path.exists() {
        fs::remove_file(&lock.path)?;
    }
    Ok(())
}

fn new_operation(
    plan: &RuntimePlan,
    confirm: bool,
    dry_run: bool,
    now: DateTime<Utc>,
) -> RuntimeOperationRecord {
    RuntimeOperationRecord {
        operation_id: plan.operation_id.clone(),
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
INSERT INTO service_runtime_operations(operation_id, object_type, object_id, action, status, actor_username, request, plan, result, error_message)
VALUES ('{op}', 'service', '{service}', '{action}', '{status}', '{actor}', '{request}'::jsonb, '{plan}'::jsonb, '{result}'::jsonb, '{error}')
ON CONFLICT(operation_id) DO UPDATE SET
    status = EXCLUDED.status,
    result = EXCLUDED.result,
    error_message = EXCLUDED.error_message,
    updated_at = NOW();
{audit_sql}
"#,
        op = sql_escape(&operation.operation_id),
        service = sql_escape(&operation.service_id),
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
            trusted_env_file(repo_root),
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
        .envs(static_compose_env(repo_root))
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

fn static_compose_env(repo_root: &Path) -> Vec<(&'static str, &'static str)> {
    if trusted_env_file(repo_root) == DEFAULT_ENV_FILE {
        return Vec::new();
    }
    vec![
        ("POSTGRES_PASSWORD", "ojos-local-postgres-password"),
        (
            "POSTGRES_DSN",
            "postgres://postgres:ojos-local-postgres-password@postgres:5432/ojos?sslmode=disable",
        ),
        ("JWT_SECRET", "ojos-local-jwt-secret"),
        ("OJOS_WORKER_TOKEN", "ojos-local-worker-token"),
        (
            "ROOT_RUNTIME_MANAGER_INTERNAL_TOKEN",
            "ojos-local-root-runtime-manager-token",
        ),
    ]
}

fn trusted_compose_services() -> BTreeSet<&'static str> {
    [
        "auth",
        "gateway",
        "root-runtime-manager",
        "problem-api",
        "judge-api",
        "judge-worker",
        "postgres",
        "redis",
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

fn print_value(
    value: &impl Serialize,
    output: &OutputMode,
    title: &str,
    lines: Vec<String>,
) -> Result<()> {
    if output.json {
        println!("{}", serde_json::to_string_pretty(value)?);
        return Ok(());
    }
    println!("{}", title);
    for line in lines {
        println!("  - {}", redact_text(&line));
    }
    Ok(())
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
            name: "Problem API".to_string(),
            kind: "backend-http".to_string(),
            lifecycle: "managed".to_string(),
            runtime: "container".to_string(),
            compose_service: "problem-api".to_string(),
            endpoint: "0.0.0.0:8083".to_string(),
            routes: vec!["/api/problem".to_string()],
            required: true,
            state: "DECLARED".to_string(),
            health: "unknown".to_string(),
            can_plan: true,
            blocked_by: vec![],
            warnings: vec![],
        }
    }

    fn repo_root() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("deploy/compose")).expect("compose dir");
        fs::write(dir.path().join(DEFAULT_COMPOSE_FILE), "services: {}\n").expect("compose file");
        dir
    }

    #[test]
    fn plan_json_uses_argv_and_ttl() {
        let dir = repo_root();
        let plan = runtime_plan(dir.path(), "restart", &service());
        assert!(plan.can_apply);
        assert!(plan.requires_confirmation);
        assert_eq!(plan.commands[0].kind, "compose");
        assert_eq!(plan.commands[0].argv[0], "docker");
        assert_eq!(plan.commands[0].argv[3], EXAMPLE_ENV_FILE);
        assert!(plan.commands[0].argv.contains(&"restart".to_string()));
        assert!(DateTime::parse_from_rfc3339(&plan.expires_at).is_ok());
    }

    #[test]
    fn validate_rejects_expired_plan() {
        let dir = repo_root();
        let mut plan = runtime_plan(dir.path(), "restart", &service());
        plan.expires_at = (Utc::now() - Duration::minutes(1)).to_rfc3339();
        let blocked = validate_plan(&plan, dir.path(), Utc::now());
        assert!(blocked.iter().any(|item| item == "plan expired"));
    }

    #[test]
    fn validate_rejects_bad_service_and_shell_meta() {
        let dir = repo_root();
        let mut plan = runtime_plan(dir.path(), "restart", &service());
        plan.service_id = "not-allowed".to_string();
        plan.allowed_targets = vec!["not-allowed".to_string()];
        plan.commands[0].argv[7] = "problem-api;bad".to_string();
        let blocked = validate_plan(&plan, dir.path(), Utc::now());
        assert!(blocked.iter().any(|item| item.contains("shell")));
        assert!(
            blocked
                .iter()
                .any(|item| item.contains("trusted compose allowlist"))
        );
    }

    #[test]
    fn validate_rejects_bad_action() {
        let dir = repo_root();
        let mut plan = runtime_plan(dir.path(), "restart", &service());
        plan.action = "exec".to_string();
        let blocked = validate_plan(&plan, dir.path(), Utc::now());
        assert!(
            blocked
                .iter()
                .any(|item| item.contains("unsupported action"))
        );
    }

    #[test]
    fn runtime_lock_blocks_concurrent_apply() {
        let dir = repo_root();
        let plan = runtime_plan(dir.path(), "restart", &service());
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
}
