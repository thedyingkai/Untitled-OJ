use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use module_installer_core::{
    Manifest, RegistrySnapshot, ServiceDecl, WorkerDecl, install_plan, package_module,
    validate_manifest, validate_manifest_file, verify_package,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
                bail!("modules directory is missing");
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
            out,
        } => write_or_print_plan(
            &runtime_plan("start", &find_runtime_service(&repo_root, &service_id)?),
            out,
        ),
        RuntimeCommands::PlanStop {
            service_id,
            repo_root,
            out,
        } => write_or_print_plan(
            &runtime_plan("stop", &find_runtime_service(&repo_root, &service_id)?),
            out,
        ),
        RuntimeCommands::PlanRestart {
            service_id,
            repo_root,
            out,
        } => write_or_print_plan(
            &runtime_plan("restart", &find_runtime_service(&repo_root, &service_id)?),
            out,
        ),
        RuntimeCommands::ApplyPlan {
            plan,
            confirm,
            dry_run,
            repo_root,
            operation_log,
            verbose,
        } => apply_runtime_plan(&plan, &repo_root, &operation_log, confirm, dry_run, verbose),
        RuntimeCommands::Operations { operation_log } => {
            print_json(&serde_json::json!({ "operations": read_operation_log(&operation_log)? }))
        }
        RuntimeCommands::Operation {
            operation_id,
            operation_log,
        } => {
            let operation = read_operation_log(&operation_log)?
                .into_iter()
                .find(|item| item.operation_id == operation_id)
                .with_context(|| format!("runtime operation not found: {}", operation_id))?;
            print_json(&operation)
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

fn write_or_print_plan(plan: &RuntimePlan, out: Option<PathBuf>) -> Result<()> {
    if let Some(path) = out {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", slash_path(parent)))?;
        }
        fs::write(&path, serde_json::to_string_pretty(plan)?)
            .with_context(|| format!("write {}", slash_path(&path)))?;
        print_json(&serde_json::json!({
            "written": true,
            "path": slash_path(&path),
            "plan_id": plan.plan_id,
            "operation_id": plan.operation_id,
            "can_apply": plan.can_apply
        }))
    } else {
        print_json(plan)
    }
}

fn apply_runtime_plan(
    plan_path: &Path,
    repo_root: &Path,
    operation_log: &Path,
    confirm: bool,
    dry_run: bool,
    verbose: bool,
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
        print_json(&operation)?;
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
        print_json(&operation)?;
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
            print_json(&operation)?;
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
            print_json(&operation)
        }
        Err(err) => {
            operation.status = "FAILED".to_string();
            operation.error_message = redact_text(&err.to_string());
            operation.result = serde_json::json!({ "error": operation.error_message });
            operation.updated_at = Utc::now().to_rfc3339();
            append_operation_log(operation_log, &operation)?;
            write_db_operation(repo_root, &operation).ok();
            print_json(&operation)?;
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
        if let Ok(value) = serde_json::from_str::<Value>(&content) {
            if let Some(expires_at) = value.get("expires_at").and_then(Value::as_str) {
                if let Ok(parsed) = DateTime::parse_from_rfc3339(expires_at) {
                    if parsed.with_timezone(&Utc) > Utc::now() {
                        bail!(
                            "runtime operation lock is held for service {}",
                            plan.service_id
                        );
                    }
                }
            }
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
        "module-installer",
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
}
