use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use module_installer_core::{
    Manifest, RegistrySnapshot, ServiceDecl, WorkerDecl, install_plan, package_module,
    validate_manifest, validate_manifest_file, verify_package,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ojosctl")]
#[command(about = "OJOS control-plane utility")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Module {
        #[command(subcommand)]
        command: ModuleCommands,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },
}

#[derive(Subcommand)]
enum ModuleCommands {
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
}

#[derive(Subcommand)]
enum RuntimeCommands {
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
    },
    PlanStop {
        service_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    PlanRestart {
        service_id: String,
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
    },
    ApplyPlan {
        plan: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Module { command } => run_module(command),
        Commands::Runtime { command } => run_runtime(command),
    }
}

fn run_module(command: ModuleCommands) -> Result<()> {
    match command {
        ModuleCommands::Discover { repo_root } => {
            let modules_dir = repo_root.join("modules");
            let mut items = Vec::new();
            if modules_dir.exists() {
                for entry in std::fs::read_dir(&modules_dir).context("read modules directory")? {
                    let entry = entry.context("read module entry")?;
                    let manifest_path = PathBuf::from("modules")
                        .join(entry.file_name())
                        .join("module.yaml");
                    if repo_root.join(&manifest_path).exists() {
                        match validate_manifest_file(&repo_root, &manifest_path) {
                            Ok(manifest) => items.push(serde_json::json!({
                                "manifest_path": manifest_path.to_string_lossy().replace('\\', "/"),
                                "module_id": manifest.id,
                                "name": manifest.name,
                                "version": manifest.version,
                                "valid": true
                            })),
                            Err(err) => items.push(serde_json::json!({
                                "manifest_path": manifest_path.to_string_lossy().replace('\\', "/"),
                                "valid": false,
                                "error": err.to_string()
                            })),
                        }
                    }
                }
            }
            print_json(&serde_json::json!({ "modules": items }))
        }
        ModuleCommands::Validate {
            manifest,
            repo_root,
        } => {
            let manifest = validate_manifest_file(&repo_root, &manifest)?;
            validate_manifest(&manifest)?;
            print_json(&serde_json::json!({ "valid": true, "manifest": manifest }))
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
            print_json(&plan)
        }
        ModuleCommands::Package { module_dir, output } => {
            let result = package_module(&module_dir, &output)?;
            print_json(&result)
        }
        ModuleCommands::Verify { package } | ModuleCommands::Inspect { package } => {
            let result = verify_package(&package)?;
            print_json(&result)
        }
        ModuleCommands::Doctor { repo_root } => {
            let modules_dir = repo_root.join("modules");
            print_json(&serde_json::json!({
                "repo_root": ".",
                "modules_dir_exists": modules_dir.is_dir(),
                "manifest_schema_versions": [1],
                "package": {
                    "format": "ojosmod",
                    "version": 1,
                    "checksum_integrity": true,
                    "signature_trust_policy": "v1"
                },
                "ok": modules_dir.is_dir()
            }))?;
            if !modules_dir.is_dir() {
                anyhow::bail!("modules directory is missing");
            }
            Ok(())
        }
    }
}

fn run_runtime(command: RuntimeCommands) -> Result<()> {
    match command {
        RuntimeCommands::Services { repo_root } => {
            let services = load_runtime_services(&repo_root)?;
            print_json(&serde_json::json!({ "services": services }))
        }
        RuntimeCommands::Service {
            service_id,
            repo_root,
        } => {
            let service = find_runtime_service(&repo_root, &service_id)?;
            print_json(&service)
        }
        RuntimeCommands::PlanStart {
            service_id,
            repo_root,
        } => {
            let service = find_runtime_service(&repo_root, &service_id)?;
            print_json(&runtime_plan("start", &service))
        }
        RuntimeCommands::PlanStop {
            service_id,
            repo_root,
        } => {
            let service = find_runtime_service(&repo_root, &service_id)?;
            print_json(&runtime_plan("stop", &service))
        }
        RuntimeCommands::PlanRestart {
            service_id,
            repo_root,
        } => {
            let service = find_runtime_service(&repo_root, &service_id)?;
            print_json(&runtime_plan("restart", &service))
        }
        RuntimeCommands::ApplyPlan { plan } => {
            print_json(&serde_json::json!({
                "plan_path": plan.to_string_lossy().replace('\\', "/"),
                "can_apply": false,
                "error": "runtime apply-plan is not implemented in L2 foundation"
            }))?;
            anyhow::bail!("runtime apply-plan is not implemented in L2 foundation")
        }
    }
}

#[derive(serde::Serialize)]
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

fn load_runtime_services(repo_root: &PathBuf) -> Result<Vec<RuntimeServiceView>> {
    let mut out = Vec::new();
    let modules_dir = repo_root.join("modules");
    if !modules_dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&modules_dir).context("read modules directory")? {
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
        warnings: vec!["ojosctl runtime is plan-only in L2 foundation".to_string()],
    }
}

fn find_runtime_service(repo_root: &PathBuf, service_id: &str) -> Result<RuntimeServiceView> {
    let service_id = service_id.trim();
    load_runtime_services(repo_root)?
        .into_iter()
        .find(|service| service.service_id == service_id)
        .with_context(|| format!("runtime service not found: {}", service_id))
}

fn runtime_plan(action: &str, service: &RuntimeServiceView) -> serde_json::Value {
    let mut blocked_by = service.blocked_by.clone();
    if service.lifecycle == "metadata" {
        blocked_by.push(format!("metadata lifecycle cannot {}", action));
    }
    let commands = if blocked_by.is_empty() {
        vec![serde_json::json!({
            "tool": "compose",
            "args": [action, service.compose_service]
        })]
    } else {
        Vec::new()
    };
    serde_json::json!({
        "plan_id": format!("runtime-{}-{}", action, service.service_id),
        "action": action,
        "service_id": service.service_id,
        "module_id": service.module_id,
        "driver": "compose",
        "can_apply": false,
        "apply_enabled": false,
        "commands": commands,
        "affected": [service.service_id],
        "blocked_by": blocked_by,
        "warnings": ["ojosctl generates runtime plans only; apply is disabled in L2 foundation"]
    })
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

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
