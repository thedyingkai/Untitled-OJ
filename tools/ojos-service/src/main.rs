mod signed_baseline;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use ojos_service::{
    check_report,
    codegen::{generate_to, verify_generated},
    compatibility::{CompatibilityReportV1, compare},
    compile, contract_bytes, discover, discover_report,
    publish::{CatalogPublishOptions, publish_catalog_v2},
    seal::{
        artifact_requirements, load_resolved_artifacts, release_lock_digest, seal,
        write_release_lock,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use signed_baseline::load_signed_baseline;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Parser)]
#[command(name = "ojos", version, about = "OJOS service authoring compiler")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Service(ServiceCommand),
}

#[derive(Debug, Args)]
struct ServiceCommand {
    #[command(subcommand)]
    command: ServiceSubcommand,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum ServiceSubcommand {
    /// Scaffold the only developer-owned service inputs.
    New {
        service_id: String,
        #[arg(long)]
        directory: Option<PathBuf>,
        #[arg(long, default_value = "Service")]
        display_name: String,
        #[arg(long, default_value = "0.1.0")]
        version: String,
    },
    /// Compile and validate all source-owned contracts.
    Check {
        #[arg(default_value = "ojos.service.yaml")]
        manifest: PathBuf,
        #[arg(long, conflicts_with = "previous_catalog")]
        previous_contract: Option<PathBuf>,
        /// Offline signed Catalog v2 publication containing the prior stable release.
        #[arg(
            long,
            requires = "previous_trust",
            conflicts_with = "previous_contract"
        )]
        previous_catalog: Option<PathBuf>,
        /// Operator-owned trust map outside `previous-catalog`; catalog-local trust is rejected.
        #[arg(long, requires = "previous_catalog")]
        previous_trust: Option<PathBuf>,
        #[arg(long, default_value = "linux")]
        target_os: String,
        #[arg(long, default_value = "x86_64")]
        target_arch: String,
        /// Require checked-in `gen/` to match deterministic compiler output.
        #[arg(long)]
        generated: bool,
    },
    /// Write deterministic compiler-owned output below `gen/`.
    Generate {
        #[arg(default_value = "ojos.service.yaml")]
        manifest: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Local authoring gate: compile, regenerate, then verify byte-for-byte.
    Dev {
        #[arg(default_value = "ojos.service.yaml")]
        manifest: PathBuf,
    },
    /// Produce generated sources and a build-input manifest for an external builder.
    Build {
        #[arg(default_value = "ojos.service.yaml")]
        manifest: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Seal resolved OCI/artifact digests into the immutable Release lock.
    Publish {
        #[arg(default_value = "ojos.service.yaml")]
        manifest: PathBuf,
        #[arg(long)]
        artifacts: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long, conflicts_with = "previous_catalog")]
        previous_contract: Option<PathBuf>,
        /// Offline signed Catalog v2 publication containing the prior stable release.
        #[arg(
            long,
            requires = "previous_trust",
            conflicts_with = "previous_contract"
        )]
        previous_catalog: Option<PathBuf>,
        /// Operator-owned trust map outside `previous-catalog`; catalog-local trust is rejected.
        #[arg(long, requires = "previous_catalog")]
        previous_trust: Option<PathBuf>,
        /// New directory containing signed Catalog v2, metadata, trust and source documents.
        #[arg(long)]
        catalog_output: PathBuf,
        /// File containing canonical padded base64 for exactly 32 Ed25519 seed bytes.
        #[arg(long)]
        signing_key_file: PathBuf,
        #[arg(long)]
        public_base_url: String,
        #[arg(long, default_value = "ojos-release-key")]
        key_id: String,
        #[arg(long, default_value = "ojos-service-contract-v3")]
        catalog_id: String,
        #[arg(long, default_value = "1.0.0")]
        min_orchestrator_version: semver::Version,
        #[arg(long, default_value = "linux")]
        target_os: String,
        #[arg(long, default_value = "x86_64")]
        target_arch: String,
    },
    /// Discover service manifests without a hand-maintained list.
    Discover {
        #[arg(default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        check_generated: bool,
    },
    /// Compatibility alias retained while old build scripts migrate.
    Compile {
        #[arg(default_value = "ojos.service.yaml")]
        manifest: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildInputV1 {
    schema_version: &'static str,
    service_id: String,
    service_version: String,
    contract_digest: String,
    generated_files: usize,
    artifact_requirements: Vec<ojos_service::seal::ArtifactRequirement>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredContract(serde_json::Value);

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Service(service) => run_service(service.command),
    }
}

fn run_service(command: ServiceSubcommand) -> Result<()> {
    match command {
        ServiceSubcommand::New {
            service_id,
            directory,
            display_name,
            version,
        } => scaffold(&service_id, directory.as_deref(), &display_name, &version),
        ServiceSubcommand::Check {
            manifest,
            previous_contract,
            previous_catalog,
            previous_trust,
            target_os,
            target_arch,
            generated,
        } => {
            let contract = compile(&manifest)?;
            let compatibility = compatibility_evaluation(
                previous_contract.as_deref(),
                previous_catalog.as_deref(),
                previous_trust.as_deref(),
                &contract,
                &orchestrator_manager::catalog_v2::TargetPlatform::new(target_os, target_arch),
            )?;
            if generated {
                verify_generated(&contract, &generation_root(&manifest, None)?)?;
            }
            let mut report = check_report(&manifest)?;
            report["generatedVerified"] = generated.into();
            if let Some((compatibility, baseline)) = compatibility {
                ensure!(
                    compatibility.compatible,
                    "breaking API change requires an API major bump: {}",
                    serde_json::to_string(&compatibility.issues)?
                );
                report["compatibility"] = serde_json::to_value(compatibility)?;
                report["compatibilityBaseline"] = baseline;
            }
            print_json(&report)
        }
        ServiceSubcommand::Generate { manifest, output }
        | ServiceSubcommand::Compile { manifest, output } => {
            let contract = compile(&manifest)?;
            let output = generation_root(&manifest, output.as_deref())?;
            let report = generate_to(&contract, &output)?;
            print_json(&json!({"status": "ok", "output": output, "generation": report}))
        }
        ServiceSubcommand::Dev { manifest } => {
            let contract = compile(&manifest)?;
            let output = generation_root(&manifest, None)?;
            generate_to(&contract, &output)?;
            let verified = verify_generated(&contract, &output)?;
            print_json(&json!({
                "status": "ready",
                "serviceId": contract.service_id,
                "output": output,
                "verification": verified,
            }))
        }
        ServiceSubcommand::Build { manifest, output } => {
            let contract = compile(&manifest)?;
            let output = generation_root(&manifest, output.as_deref())?;
            let generation = generate_to(&contract, &output)?;
            verify_generated(&contract, &output)?;
            let build_input = BuildInputV1 {
                schema_version: "ojos.dev/build-input/v1",
                service_id: contract.service_id.clone(),
                service_version: contract.service_version.to_string(),
                contract_digest: digest(&contract_bytes(&contract)?),
                generated_files: generation.files.len(),
                artifact_requirements: artifact_requirements(&contract)?,
            };
            let build_path = output.join("build-input.json");
            write_json(&build_path, &build_input)?;
            print_json(&json!({"status": "ready-for-builder", "buildInput": build_path}))
        }
        ServiceSubcommand::Publish {
            manifest,
            artifacts,
            output,
            previous_contract,
            previous_catalog,
            previous_trust,
            catalog_output,
            signing_key_file,
            public_base_url,
            key_id,
            catalog_id,
            min_orchestrator_version,
            target_os,
            target_arch,
        } => {
            let contract = compile(&manifest)?;
            let compatibility = compatibility_evaluation(
                previous_contract.as_deref(),
                previous_catalog.as_deref(),
                previous_trust.as_deref(),
                &contract,
                &orchestrator_manager::catalog_v2::TargetPlatform::new(
                    target_os.clone(),
                    target_arch.clone(),
                ),
            )?;
            if let Some((report, _)) = &compatibility {
                ensure!(
                    report.compatible,
                    "breaking API change requires an API major bump: {}",
                    serde_json::to_string(&report.issues)?
                );
            }
            let generated_root = generation_root(&manifest, None)?;
            verify_generated(&contract, &generated_root)
                .context("run `ojos service build` and commit deterministic output first")?;
            let resolved = load_resolved_artifacts(&artifacts)?;
            let lock = seal(&contract, &resolved)?;
            let output = output.unwrap_or_else(|| generated_root.join("release.lock.json"));
            write_release_lock(&lock, &output)?;
            let catalog = publish_catalog_v2(
                &contract,
                &lock,
                &manifest,
                &CatalogPublishOptions {
                    output_directory: catalog_output,
                    signing_key_file,
                    public_base_url,
                    key_id,
                    catalog_id,
                    min_orchestrator_version,
                    target_os,
                    target_arch,
                },
            )?;
            print_json(&json!({
                "status": "published",
                "releaseLock": output,
                "releaseLockDigest": release_lock_digest(&lock)?,
                "catalogV2": catalog,
                "compatibility": compatibility.as_ref().map(|(report, _)| report),
                "compatibilityBaseline": compatibility.map(|(_, baseline)| baseline),
            }))
        }
        ServiceSubcommand::Discover {
            root,
            check_generated,
        } => {
            if check_generated {
                let manifests = discover(&root)?;
                let mut checked = Vec::new();
                for manifest in manifests {
                    let contract = compile(&manifest)?;
                    verify_generated(&contract, &generation_root(&manifest, None)?)?;
                    checked.push(contract.service_id);
                }
                print_json(&json!({
                    "schemaVersion": "ojos.dev/discovery-gate/v1",
                    "status": "ok",
                    "services": checked,
                }))
            } else {
                print_json(&discover_report(&root)?)
            }
        }
    }
}

fn compatibility_evaluation(
    previous_contract: Option<&Path>,
    previous_catalog: Option<&Path>,
    previous_trust: Option<&Path>,
    current: &ojos_service::ServiceContractV3,
    target: &orchestrator_manager::catalog_v2::TargetPlatform,
) -> Result<Option<(CompatibilityReportV1, serde_json::Value)>> {
    ensure!(
        previous_contract.is_none() || previous_catalog.is_none(),
        "previous-contract and previous-catalog are mutually exclusive"
    );
    if let Some(directory) = previous_catalog {
        let trust = previous_trust.context("previous-trust is required with previous-catalog")?;
        let baseline = load_signed_baseline(directory, trust, current, target)?;
        let report = compare(&baseline.contract, current).map_err(anyhow::Error::msg)?;
        return Ok(Some((report, serde_json::to_value(baseline.report)?)));
    }
    ensure!(
        previous_trust.is_none(),
        "previous-trust requires previous-catalog"
    );
    let Some(path) = previous_contract else {
        return Ok(None);
    };
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    // Deserialize via Value first so unknown/tampered fields are rejected by
    // ServiceContractV3's strict serde contract.
    let value: StoredContract = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse previous contract {}", path.display()))?;
    let previous = serde_json::from_value(value.0)
        .with_context(|| format!("validate previous contract {}", path.display()))?;
    let report = compare(&previous, current).map_err(anyhow::Error::msg)?;
    Ok(Some((
        report,
        json!({
            "trust": "untrusted-local-migration-baseline",
            "path": path,
            "warning": "previous-contract is retained for one migration version and is not a production trust root"
        }),
    )))
}

fn generation_root(manifest: &Path, output: Option<&Path>) -> Result<PathBuf> {
    if let Some(output) = output {
        return Ok(output.to_path_buf());
    }
    let parent = manifest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(parent.join("gen"))
}

fn scaffold(
    service_id: &str,
    directory: Option<&Path>,
    display_name: &str,
    version: &str,
) -> Result<()> {
    validate_service_id(service_id)?;
    let _: semver::Version = version
        .parse()
        .with_context(|| format!("version {version} must be semver"))?;
    ensure!(
        !display_name.trim().is_empty(),
        "display-name cannot be empty"
    );
    let root = directory
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("services").join(service_id));
    ensure!(!root.exists(), "{} already exists", root.display());
    fs::create_dir_all(root.join("api"))?;

    let manifest = format!(
        "apiVersion: ojos.dev/v1\nkind: Service\nmetadata:\n  id: {service_id}\n  version: {version}\n  displayName: {display_name:?}\nruntime:\n  profile: standard-container-v1\n  artifact: runtime\n  httpPort: 8080\n  health:\n    path: /healthz\nprovides:\n  apis:\n    - document: api/openapi.yaml\n"
    );
    let openapi = format!(
        "openapi: 3.1.0\ninfo:\n  title: {display_name:?}\n  version: {version}\nx-ojos-api-id: {service_id}.api\nx-ojos-service:\n  id: {service_id}\n  version: {version}\npaths:\n  /healthz:\n    get:\n      operationId: healthReady\n      x-ojos-audience: internal\n      security: []\n      responses:\n        '200':\n          description: Ready\n"
    );
    write_new(&root.join("ojos.service.yaml"), manifest.as_bytes())?;
    write_new(&root.join("api/openapi.yaml"), openapi.as_bytes())?;
    print_json(&json!({"status": "created", "serviceId": service_id, "directory": root}))
}

fn validate_service_id(value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if !valid {
        bail!("service-id must be a lowercase DNS label")
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_build_and_generated_gate_are_end_to_end() {
        let temp = tempfile::tempdir().unwrap();
        let service = temp.path().join("contest-service");
        scaffold("contest-service", Some(&service), "Contest", "0.1.0").unwrap();
        let manifest = service.join("ojos.service.yaml");
        let contract = compile(&manifest).unwrap();
        let generated_root = generation_root(&manifest, None).unwrap();
        generate_to(&contract, &generated_root).unwrap();
        verify_generated(&contract, &generated_root).unwrap();

        fs::write(generated_root.join("ts/src/client.ts"), "tampered\n").unwrap();
        assert!(verify_generated(&contract, &generated_root).is_err());
    }

    #[test]
    fn signed_catalog_cli_requires_external_trust_and_conflicts_with_local_baseline() {
        assert!(
            Cli::try_parse_from([
                "ojos",
                "service",
                "check",
                "ojos.service.yaml",
                "--previous-catalog",
                "catalog"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "ojos",
                "service",
                "check",
                "ojos.service.yaml",
                "--previous-catalog",
                "catalog",
                "--previous-trust",
                "operator-trust.json",
                "--previous-contract",
                "contract.json"
            ])
            .is_err()
        );
    }

    #[test]
    fn local_previous_contract_is_explicitly_reported_as_untrusted_migration_input() {
        let temp = tempfile::tempdir().unwrap();
        let service = temp.path().join("contest-service");
        scaffold("contest-service", Some(&service), "Contest", "0.1.0").unwrap();
        let mut current = compile(&service.join("ojos.service.yaml")).unwrap();
        let previous_path = temp.path().join("previous.contract.json");
        fs::write(&previous_path, contract_bytes(&current).unwrap()).unwrap();
        current.service_version = semver::Version::parse("0.2.0").unwrap();

        let (_, baseline) = compatibility_evaluation(
            Some(&previous_path),
            None,
            None,
            &current,
            &orchestrator_manager::catalog_v2::TargetPlatform::new("linux", "x86_64"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(baseline["trust"], "untrusted-local-migration-baseline");
    }
}
