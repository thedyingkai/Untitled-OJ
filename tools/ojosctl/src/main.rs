use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use module_installer_core::{
    RegistrySnapshot, install_plan, package_module, validate_manifest, validate_manifest_file,
    verify_package,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Module { command } => run_module(command),
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

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
