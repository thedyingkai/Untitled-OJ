use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::Parser;
use ed25519_dalek::{Signer, SigningKey};
use orchestrator_core::{SERVICE_CONTRACT_VERSION, ServiceReleaseContract};
use orchestrator_manager::catalog_v2::{
    CatalogModuleV2, CatalogReleaseV2, CatalogTrustStore, CatalogV2, Ed25519Signature,
    MetadataPackageV2, OciImageReference, ReleaseChannel, ReleaseDependencyV2, TargetPlatform,
};
use semver::{Version, VersionReq};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(
    about = "Build and self-verify a signed Catalog v2 entry from a Service Contract v2 release"
)]
struct Arguments {
    /// New output directory. Existing directories are never overwritten.
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    release_manifest: PathBuf,
    /// File containing canonical padded base64 for exactly 32 Ed25519 seed bytes.
    #[arg(long)]
    signing_key_file: PathBuf,
    #[arg(long)]
    public_base_url: String,
    /// Exact immutable repository@sha256:<64 lowercase hex> image reference.
    #[arg(long)]
    oci_image: String,
    /// Additional Service Contract v2 manifests included in the same signed Catalog.
    /// Each occurrence is paired by position with --additional-oci-image.
    #[arg(long = "additional-release-manifest")]
    additional_release_manifests: Vec<PathBuf>,
    /// Digest-pinned OCI image paired with --additional-release-manifest.
    #[arg(long = "additional-oci-image")]
    additional_oci_images: Vec<String>,
    #[arg(long, default_value = "ojos-release-key")]
    key_id: String,
    #[arg(long, default_value = "ojos-service-contract-v2")]
    catalog_id: String,
    #[arg(long, default_value = "1.0.0")]
    min_orchestrator_version: Version,
    #[arg(long, default_value = "linux")]
    target_os: String,
    #[arg(long, default_value = "x86_64")]
    target_arch: String,
}

#[derive(Debug)]
struct PreparedRelease {
    service_id: String,
    version: Version,
    description: String,
    kind: String,
    dependencies: Vec<String>,
    image: OciImageReference,
    metadata_relative: String,
    metadata_url: String,
    metadata_bytes: Vec<u8>,
    metadata_sha256: String,
}

fn main() -> Result<()> {
    let result = generate(Arguments::parse())?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn generate(arguments: Arguments) -> Result<Value> {
    let public_base_url = arguments.public_base_url.trim_end_matches('/');
    ensure!(
        public_base_url.starts_with("https://")
            && !public_base_url.chars().any(char::is_whitespace),
        "public_base_url must be HTTPS and contain no whitespace"
    );
    ensure!(
        !arguments.output.exists(),
        "output already exists; signed Catalog generation never overwrites files"
    );

    ensure!(
        arguments.additional_release_manifests.len() == arguments.additional_oci_images.len(),
        "every --additional-release-manifest requires one positional --additional-oci-image"
    );
    let signing_key = load_signing_key(&arguments.signing_key_file)?;
    let root_image_text = arguments.oci_image.clone();
    let mut requested = vec![(arguments.release_manifest.clone(), root_image_text.clone())];
    requested.extend(
        arguments
            .additional_release_manifests
            .iter()
            .cloned()
            .zip(arguments.additional_oci_images.iter().cloned()),
    );
    let mut prepared = Vec::with_capacity(requested.len());
    for (manifest_path, image_text) in requested {
        let image: OciImageReference = image_text.parse().with_context(|| {
            format!(
                "OCI image for {} must be repository@sha256:<64 lowercase hex>",
                manifest_path.display()
            )
        })?;
        let document = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let contract = ServiceReleaseContract::from_yaml_str(&document).with_context(|| {
            format!(
                "validate Service Contract release {}",
                manifest_path.display()
            )
        })?;
        ensure!(
            contract.contract_version == SERVICE_CONTRACT_VERSION,
            "production Catalog generation requires Service Contract v2: {}",
            manifest_path.display()
        );
        ensure!(
            contract.release.runtime.kind == "image",
            "Catalog module runtime must be an OCI image: {}",
            manifest_path.display()
        );

        let service_id = contract.release.service_name.clone();
        let version = Version::parse(&contract.release.version).with_context(|| {
            format!("release version is not semver: {}", manifest_path.display())
        })?;
        ensure!(
            !prepared.iter().any(|item: &PreparedRelease| {
                item.service_id == service_id && item.version == version
            }),
            "duplicate Catalog release {service_id}@{version}"
        );
        let metadata_name = format!("{service_id}-{version}.release.json");
        let metadata_relative = format!("metadata/{metadata_name}");
        let metadata_url = format!("{public_base_url}/{metadata_relative}");

        // Catalog metadata is the normalized, validated v2 release. The catalog's
        // metadata SHA-256 is authoritative, avoiding a self-referential checksum
        // inside the metadata document itself.
        let mut metadata = contract.to_json_value()?;
        let object = metadata
            .as_object_mut()
            .context("normalized release metadata must be an object")?;
        object.insert(
            "source".to_string(),
            json!({"kind": "url", "url": metadata_url, "checksum": ""}),
        );
        let runtime = object
            .get_mut("runtime")
            .and_then(Value::as_object_mut)
            .context("normalized release metadata has no runtime object")?;
        runtime.insert("kind".to_string(), Value::String("image".to_string()));
        runtime.insert("image".to_string(), Value::String(image_text.clone()));
        runtime.insert("binary".to_string(), Value::String(String::new()));
        runtime.insert("system_service".to_string(), Value::String(String::new()));
        ServiceReleaseContract::from_json_value(metadata.clone())
            .context("validate generated digest-pinned release metadata")?;
        let metadata_bytes = serde_json::to_vec_pretty(&metadata)?;
        let metadata_sha256 = format!("sha256:{:x}", Sha256::digest(&metadata_bytes));
        prepared.push(PreparedRelease {
            service_id,
            version,
            description: contract.release.description.clone(),
            kind: contract.release.service_type.clone(),
            dependencies: contract.release.dependencies.clone(),
            image,
            metadata_relative,
            metadata_url,
            metadata_bytes,
            metadata_sha256,
        });
    }

    let root_service_id = prepared[0].service_id.clone();
    let root_version = prepared[0].version.clone();
    let mut versions_by_module: BTreeMap<&str, Vec<&Version>> = BTreeMap::new();
    for release in &prepared {
        versions_by_module
            .entry(release.service_id.as_str())
            .or_default()
            .push(&release.version);
    }
    let mut modules: BTreeMap<String, CatalogModuleV2> = BTreeMap::new();
    for release in &prepared {
        let dependencies = release
            .dependencies
            .iter()
            .map(|module_id| {
                let versions = versions_by_module
                    .get(module_id.as_str())
                    .with_context(|| {
                        format!(
                            "Catalog release {}@{} depends on missing module {module_id}",
                            release.service_id, release.version
                        )
                    })?;
                ensure!(
                    versions.len() == 1,
                    "Catalog release {}@{} dependency {module_id} is ambiguous across {} versions",
                    release.service_id,
                    release.version,
                    versions.len()
                );
                Ok(ReleaseDependencyV2 {
                    module_id: module_id.clone(),
                    requirement: VersionReq::parse(&format!("={}", versions[0]))?,
                    channel: ReleaseChannel::Stable,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let module = modules
            .entry(release.service_id.clone())
            .or_insert_with(|| CatalogModuleV2 {
                id: release.service_id.clone(),
                name: release.service_id.clone(),
                description: release.description.clone(),
                kind: release.kind.clone(),
                tags: vec![
                    "service-contract-v2".to_string(),
                    "digest-pinned".to_string(),
                ],
                releases: Vec::new(),
            });
        ensure!(
            module.description == release.description && module.kind == release.kind,
            "Catalog module {} releases disagree on description or kind",
            release.service_id
        );
        module.releases.push(CatalogReleaseV2 {
            version: release.version.clone(),
            channel: ReleaseChannel::Stable,
            platforms: vec![TargetPlatform::new(
                arguments.target_os.clone(),
                arguments.target_arch.clone(),
            )],
            min_orchestrator_version: arguments.min_orchestrator_version.clone(),
            dependencies,
            runtime_capabilities: Vec::new(),
            metadata: MetadataPackageV2 {
                url: release.metadata_url.clone(),
                sha256: release.metadata_sha256.parse()?,
            },
            oci_image: release.image.clone(),
        });
    }
    for module in modules.values_mut() {
        module
            .releases
            .sort_by(|left, right| left.version.cmp(&right.version));
    }

    let mut catalog = CatalogV2 {
        schema_version: 2,
        id: arguments.catalog_id.clone(),
        name: format!("OJOS {root_service_id} aggregate signed releases"),
        modules: modules.into_values().collect(),
        signatures: Vec::new(),
    };
    let signature = signing_key.sign(&catalog.signing_payload_jcs()?);
    catalog.signatures.push(Ed25519Signature {
        key_id: arguments.key_id.clone(),
        algorithm: "Ed25519".to_string(),
        signature: STANDARD.encode(signature.to_bytes()),
    });
    let public_key = STANDARD.encode(signing_key.verifying_key().to_bytes());
    let mut trust = CatalogTrustStore::new();
    trust.insert_base64(arguments.key_id.clone(), &public_key)?;
    catalog
        .validate_trusted(&trust)
        .context("self-verify signed Catalog v2")?;

    let parent = arguments
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("create Catalog output parent")?;
    let output_name = arguments
        .output
        .file_name()
        .and_then(|value| value.to_str())
        .context("output directory must have a UTF-8 name")?;
    let staging = parent.join(format!(
        ".{output_name}.pending-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir(&staging).context("create Catalog staging directory")?;
    fs::create_dir(staging.join("metadata")).context("create metadata directory")?;
    for release in &prepared {
        write_new(
            &staging.join(&release.metadata_relative),
            &release.metadata_bytes,
        )?;
    }
    write_new(
        &staging.join("catalog.json"),
        &serde_json::to_vec_pretty(&catalog)?,
    )?;
    write_new(
        &staging.join("trust.json"),
        &serde_json::to_vec_pretty(&json!({arguments.key_id.clone(): public_key}))?,
    )?;
    write_new(
        &staging.join("catalog-source.json"),
        &serde_json::to_vec_pretty(&json!([{
            "id": arguments.catalog_id,
            "url": format!("{public_base_url}/catalog.json"),
            "required_key_id": arguments.key_id,
            "auth_secret_ref": "",
            "enabled": true,
            "offline_oci_layouts": {}
        }]))?,
    )?;
    fs::rename(&staging, &arguments.output)
        .context("publish complete signed Catalog atomically")?;

    Ok(json!({
        "status": "ok",
        "service_id": root_service_id,
        "version": root_version,
        "oci_image": root_image_text,
        "release_count": prepared.len(),
        "catalog": arguments.output.join("catalog.json"),
        "metadata": arguments.output.join(&prepared[0].metadata_relative),
        "trust": arguments.output.join("trust.json")
    }))
}

fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let encoded = fs::read_to_string(path).context("read Ed25519 signing key file")?;
    let bytes = STANDARD
        .decode(encoded.trim())
        .context("signing key file must contain canonical padded base64")?;
    ensure!(
        STANDARD.encode(&bytes) == encoded.trim(),
        "signing key must use canonical padded base64"
    );
    let seed: [u8; 32] = bytes.try_into().map_err(|value: Vec<u8>| {
        anyhow::anyhow!("signing key is {} bytes; expected 32", value.len())
    })?;
    Ok(SigningKey::from_bytes(&seed))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))
}
