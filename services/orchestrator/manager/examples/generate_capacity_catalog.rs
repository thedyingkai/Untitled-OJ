use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::Parser;
use ed25519_dalek::{Signer, SigningKey};
use orchestrator_manager::catalog_v2::{
    CatalogModuleV2, CatalogReleaseV2, CatalogTrustStore, CatalogV2, Ed25519Signature,
    MetadataPackageV2, OciImageReference, ReleaseChannel, RuntimeCapabilityV2, TargetPlatform,
};
use semver::Version;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(about = "Generate the immutable signed 20-service production-capacity Catalog v2")]
struct Arguments {
    #[arg(long)]
    output: PathBuf,
    /// File containing canonical padded base64 for exactly 32 raw Ed25519 seed bytes.
    #[arg(long)]
    signing_key_file: PathBuf,
    #[arg(long, default_value = "capacity-fixture-key")]
    key_id: String,
    /// HTTPS directory from which catalog.json and metadata/*.release.json are served.
    #[arg(long)]
    public_base_url: String,
    /// Exact immutable repository@sha256:<64 lowercase hex> fixture image.
    #[arg(long)]
    oci_image: String,
    #[arg(long)]
    candidate_sha: String,
    #[arg(long, default_value = "capacity-fixture")]
    catalog_id: String,
    #[arg(long, default_value = "capacity-primary")]
    topology_id: String,
    #[arg(long, default_value_t = 20)]
    service_count: usize,
}

fn main() -> Result<()> {
    generate_fixture(Arguments::parse(), None)
}

fn generate_fixture(arguments: Arguments, fail_after_writes: Option<usize>) -> Result<()> {
    ensure!(
        arguments.service_count == 20,
        "production fixture requires exactly 20 services"
    );
    ensure!(
        arguments.candidate_sha.len() == 40
            && arguments
                .candidate_sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "candidate_sha must be exactly 40 lowercase hexadecimal characters"
    );
    let candidate_sha = arguments.candidate_sha;
    let public_base_url = arguments.public_base_url.trim_end_matches('/');
    ensure!(
        public_base_url.starts_with("https://")
            && !public_base_url.chars().any(char::is_whitespace),
        "public_base_url must be HTTPS and contain no whitespace"
    );
    let image: OciImageReference = arguments
        .oci_image
        .parse()
        .context("OCI image must be repository@sha256:<64 lowercase hex>")?;
    let signing_key = load_signing_key(&arguments.signing_key_file)?;
    recover_stale_staging_directories(&arguments.output)?;
    let staging = create_unique_sibling(&arguments.output, "pending")?;
    fs::create_dir(staging.join("metadata")).context("create capacity Catalog output")?;
    let mut expected_files = BTreeMap::new();
    let mut writes = 0_usize;

    let mut modules = Vec::with_capacity(arguments.service_count);
    let mut fixture_services = Vec::with_capacity(arguments.service_count);
    for index in 0..arguments.service_count {
        let service_id = format!("capacity-{index:02}");
        let metadata_name = format!("{service_id}-1.0.0.release.json");
        let metadata_relative = format!("metadata/{metadata_name}");
        let metadata_url = format!("{public_base_url}/{metadata_relative}");
        let metadata = json!({
            "schema_version": 1,
            "service_name": service_id,
            "version": "1.0.0",
            "description": "production capacity fixture; no synthetic runtime projection",
            "service_type": "backend-api",
            "source": {
                "kind": "url",
                "url": metadata_url,
                "checksum": format!("sha256:{}", "0".repeat(64))
            },
            "runtime": {
                "kind": "image",
                "image": arguments.oci_image,
                "command": "",
                "args": [],
                "env": {
                    "OJOS_CAPACITY_CANDIDATE_SHA": candidate_sha,
                    "OJOS_CAPACITY_SERVICE_ID": service_id,
                    "OJOS_CAPACITY_PROBE_MIN_PORT": "20000",
                    "OJOS_CAPACITY_PROBE_MAX_PORT": "20199",
                    "OJOS_CAPACITY_PROBE_TIMEOUT_SECONDS": "2"
                }
            },
            "backend": {"protocol": "http", "port": 8080, "health_path": "/health"},
            "migrations": [],
            "permissions": [],
            "routes": [],
            "apis": [{
                "api_id": "orchestrator.link-probe.v1",
                "protocol": "http",
                "port_name": "default",
                "path_prefix": "/probe",
                "methods": ["GET"],
                "visibility": "global",
                "auth_mode": "public",
                "permission": "public",
                "stability": "stable",
                "version": "v1"
            }],
            "redis": [],
            "storage": [],
            "dependencies": [],
            "required_apis": [],
            "config_schema": {},
            "secrets": []
        });
        let metadata_bytes = serde_json::to_vec_pretty(&metadata)?;
        write_generated(
            &staging,
            &metadata_relative,
            &metadata_bytes,
            &mut expected_files,
            &mut writes,
            fail_after_writes,
        )?;
        let metadata_sha256 = format!("sha256:{:x}", Sha256::digest(&metadata_bytes));
        modules.push(CatalogModuleV2 {
            id: service_id.clone(),
            name: format!("Capacity fixture {index:02}"),
            description: "digest-pinned real Docker Engine capacity workload".to_string(),
            kind: "backend-api".to_string(),
            tags: vec!["capacity".to_string(), "production-evidence".to_string()],
            releases: vec![CatalogReleaseV2 {
                version: Version::parse("1.0.0")?,
                channel: ReleaseChannel::Stable,
                platforms: vec![TargetPlatform::new("linux", "x86_64")],
                min_orchestrator_version: Version::parse("1.0.0")?,
                dependencies: vec![],
                runtime_capabilities: vec![RuntimeCapabilityV2::LinkProbeV1],
                metadata: MetadataPackageV2 {
                    url: metadata_url,
                    sha256: metadata_sha256.parse()?,
                },
                oci_image: image.clone(),
            }],
        });
        fixture_services.push(json!({
            "service_id": service_id,
            "oci_image": arguments.oci_image,
        }));
    }

    let mut catalog = CatalogV2 {
        schema_version: 2,
        id: arguments.catalog_id.clone(),
        name: "OJOS production capacity fixture".to_string(),
        modules,
        signatures: vec![],
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

    write_generated(
        &staging,
        "catalog.json",
        &serde_json::to_vec_pretty(&catalog)?,
        &mut expected_files,
        &mut writes,
        fail_after_writes,
    )?;
    write_generated(
        &staging,
        "trust.json",
        &serde_json::to_vec_pretty(&json!({arguments.key_id.clone(): public_key}))?,
        &mut expected_files,
        &mut writes,
        fail_after_writes,
    )?;
    write_generated(
        &staging,
        "catalog-source.json",
        &serde_json::to_vec_pretty(&json!([{
            "id": arguments.catalog_id,
            "url": format!("{public_base_url}/catalog.json"),
            "required_key_id": arguments.key_id,
            "auth_secret_ref": "",
            "enabled": true,
            "offline_oci_layouts": {}
        }]))?,
        &mut expected_files,
        &mut writes,
        fail_after_writes,
    )?;
    write_generated(
        &staging,
        "capacity-fixture.json",
        &serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "candidate_sha": candidate_sha,
            "catalog_source_id": catalog.id,
            "version": "1.0.0",
            "channel": "stable",
            "topology_id": arguments.topology_id,
            "services": fixture_services
        }))?,
        &mut expected_files,
        &mut writes,
        fail_after_writes,
    )?;
    inject_write_failure(fail_after_writes, writes)?;
    validate_generated_tree(&staging, &expected_files)?;
    publish_staging(&staging, &arguments.output, &expected_files)?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "status": "ok",
            "catalog": arguments.output.join("catalog.json"),
            "fixture": arguments.output.join("capacity-fixture.json"),
            "public_key": arguments.output.join("trust.json"),
            "candidate_sha": candidate_sha,
            "oci_image": arguments.oci_image,
            "services": arguments.service_count
        }))?
    );
    Ok(())
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
        .with_context(|| format!("create new signed fixture file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    sync_directory(path.parent().context("fixture file has no parent")?)
}

fn write_generated(
    root: &Path,
    relative: &str,
    bytes: &[u8],
    expected: &mut BTreeMap<String, Vec<u8>>,
    writes: &mut usize,
    fail_after_writes: Option<usize>,
) -> Result<()> {
    inject_write_failure(fail_after_writes, *writes)?;
    ensure!(
        expected
            .insert(relative.to_string(), bytes.to_vec())
            .is_none(),
        "duplicate generated fixture path {relative}"
    );
    write_new(&root.join(relative), bytes)?;
    *writes += 1;
    Ok(())
}

fn inject_write_failure(fail_after_writes: Option<usize>, writes: usize) -> Result<()> {
    if fail_after_writes == Some(writes) {
        bail!("injected fixture generation failure after {writes} writes");
    }
    Ok(())
}

fn output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sibling_prefix(path: &Path, label: &str) -> Result<String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("fixture output must have a UTF-8 final path component")?;
    Ok(format!(".{name}.{label}-"))
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn create_unique_sibling(path: &Path, label: &str) -> Result<PathBuf> {
    let parent = output_parent(path);
    fs::create_dir_all(parent).context("create fixture output parent")?;
    let prefix = sibling_prefix(path, label)?;
    for attempt in 0..1_000_u32 {
        let candidate = parent.join(format!(
            "{prefix}{}-{}-{attempt}",
            std::process::id(),
            unique_suffix()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                sync_directory(parent)?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create fixture staging directory"),
        }
    }
    bail!("could not allocate a unique fixture staging directory")
}

fn available_sibling(path: &Path, label: &str) -> Result<PathBuf> {
    let parent = output_parent(path);
    let prefix = sibling_prefix(path, label)?;
    for attempt in 0..1_000_u32 {
        let candidate = parent.join(format!(
            "{prefix}{}-{}-{attempt}",
            std::process::id(),
            unique_suffix()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("could not allocate a unique fixture recovery path")
}

fn recover_stale_staging_directories(output: &Path) -> Result<()> {
    let parent = output_parent(output);
    fs::create_dir_all(parent).context("create fixture output parent")?;
    let pending_prefix = sibling_prefix(output, "pending")?;
    for entry in fs::read_dir(parent).context("enumerate fixture output parent")? {
        let entry = entry.context("read fixture output sibling")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&pending_prefix) {
            continue;
        }
        let recovered = available_sibling(output, "recovered")?;
        fs::rename(entry.path(), &recovered).with_context(|| {
            format!(
                "quarantine stale fixture staging path as {}",
                recovered.display()
            )
        })?;
    }
    sync_directory(parent)
}

fn collect_tree(root: &Path) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry.context("read generated fixture entry")?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "fixture tree contains a symlink"
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("fixture tree contains a non-UTF-8 name"))?;
        if metadata.is_file() {
            paths.insert(name);
            continue;
        }
        ensure!(
            metadata.is_dir() && name == "metadata",
            "unexpected fixture entry {name}"
        );
        for child in fs::read_dir(entry.path())? {
            let child = child?;
            let child_metadata = fs::symlink_metadata(child.path())?;
            ensure!(
                child_metadata.is_file(),
                "fixture metadata entry is not a file"
            );
            ensure!(
                !child_metadata.file_type().is_symlink(),
                "fixture metadata contains a symlink"
            );
            let child_name = child
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("fixture metadata contains a non-UTF-8 name"))?;
            paths.insert(format!("metadata/{child_name}"));
        }
    }
    Ok(paths)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    ensure!(
        metadata.len() <= 8 * 1024 * 1024,
        "fixture document is oversized"
    );
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn load_trust_store(path: &Path) -> Result<CatalogTrustStore> {
    let encoded: BTreeMap<String, String> = serde_json::from_slice(&read_bounded(path)?)
        .context("parse generated Catalog trust store")?;
    ensure!(
        !encoded.is_empty(),
        "generated Catalog trust store is empty"
    );
    let mut trust = CatalogTrustStore::new();
    for (key_id, public_key) in encoded {
        trust.insert_base64(key_id, &public_key)?;
    }
    Ok(trust)
}

fn validate_signed_fixture(root: &Path) -> Result<()> {
    let catalog: CatalogV2 = serde_json::from_slice(&read_bounded(&root.join("catalog.json"))?)
        .context("parse generated Catalog v2")?;
    let trust = load_trust_store(&root.join("trust.json"))?;
    catalog
        .validate_trusted(&trust)
        .context("validate generated Catalog signature")?;
    let source: serde_json::Value =
        serde_json::from_slice(&read_bounded(&root.join("catalog-source.json"))?)?;
    ensure!(source.as_array().is_some_and(|items| items.len() == 1));
    let fixture: serde_json::Value =
        serde_json::from_slice(&read_bounded(&root.join("capacity-fixture.json"))?)?;
    ensure!(
        fixture["services"]
            .as_array()
            .is_some_and(|items| items.len() == catalog.modules.len()),
        "fixture service count does not match the Catalog"
    );

    let mut expected_paths: BTreeSet<String> = [
        "catalog.json".to_string(),
        "trust.json".to_string(),
        "catalog-source.json".to_string(),
        "capacity-fixture.json".to_string(),
    ]
    .into_iter()
    .collect();
    for module in &catalog.modules {
        ensure!(
            module.releases.len() == 1,
            "capacity module must have one release"
        );
        let release = &module.releases[0];
        ensure!(
            release.runtime_capabilities == [RuntimeCapabilityV2::LinkProbeV1],
            "capacity release must declare the signed link-probe-v1 runtime capability"
        );
        let metadata_name = release
            .metadata
            .url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty() && !name.contains('/') && !name.contains('\\'))
            .context("Catalog metadata URL has no safe filename")?;
        let relative = format!("metadata/{metadata_name}");
        let bytes = read_bounded(&root.join(&relative))?;
        ensure!(
            format!("sha256:{:x}", Sha256::digest(&bytes)) == release.metadata.sha256.to_string(),
            "metadata checksum does not match the signed Catalog"
        );
        let manifest: orchestrator_legacy::ServiceReleaseManifest = serde_json::from_slice(&bytes)?;
        orchestrator_legacy::validate_service_release(&manifest)?;
        ensure!(
            orchestrator_legacy::release_supports_link_probe_v1(&manifest),
            "capacity metadata must expose the exact orchestrator.link-probe.v1 API"
        );
        ensure!(
            expected_paths.insert(relative),
            "duplicate Catalog metadata path"
        );
    }
    ensure!(
        collect_tree(root)? == expected_paths,
        "fixture tree has missing or unexpected files"
    );
    Ok(())
}

fn tree_matches(root: &Path, expected: &BTreeMap<String, Vec<u8>>) -> Result<bool> {
    if collect_tree(root)? != expected.keys().cloned().collect() {
        return Ok(false);
    }
    for (relative, bytes) in expected {
        if read_bounded(&root.join(relative))? != *bytes {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_generated_tree(root: &Path, expected: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    ensure!(
        tree_matches(root, expected)?,
        "generated fixture bytes changed on disk"
    );
    validate_signed_fixture(root)?;
    sync_directory(&root.join("metadata"))?;
    sync_directory(root)
}

fn publish_staging(
    staging: &Path,
    output: &Path,
    expected: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if output.exists() {
        if validate_signed_fixture(output).is_ok() {
            ensure!(
                tree_matches(output, expected)?,
                "refusing to overwrite a verified signed fixture with different content"
            );
            fs::remove_dir_all(staging).context("remove redundant verified staging tree")?;
            sync_directory(output_parent(output))?;
            return Ok(());
        }
        let recovered = available_sibling(output, "rejected")?;
        fs::rename(output, &recovered).with_context(|| {
            format!(
                "move incomplete fixture output to recoverable path {}",
                recovered.display()
            )
        })?;
        sync_directory(output_parent(output))?;
    }
    fs::rename(staging, output).context("atomically publish verified fixture tree")?;
    sync_directory(output_parent(output))?;
    validate_generated_tree(output, expected)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open directory {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn arguments(root: &Path, candidate: char) -> Arguments {
        let signing_key_file = root.join("signing-key.b64");
        fs::write(&signing_key_file, STANDARD.encode([7_u8; 32])).unwrap();
        Arguments {
            output: root.join("fixture"),
            signing_key_file,
            key_id: "capacity-fixture-key".to_string(),
            public_base_url: "https://capacity.example.test/catalog".to_string(),
            oci_image: format!("registry.example/capacity@sha256:{}", "a".repeat(64)),
            candidate_sha: candidate.to_string().repeat(40),
            catalog_id: "capacity-fixture".to_string(),
            topology_id: "capacity-primary".to_string(),
            service_count: 20,
        }
    }

    #[test]
    fn every_write_boundary_is_recoverable_and_rerun_is_idempotent() {
        for boundary in 0..=24 {
            let root = tempdir().unwrap();
            let failed = generate_fixture(arguments(root.path(), 'a'), Some(boundary));
            assert!(
                failed.is_err(),
                "boundary {boundary} unexpectedly completed"
            );
            assert!(!root.path().join("fixture").exists());

            generate_fixture(arguments(root.path(), 'a'), None).unwrap();
            validate_signed_fixture(&root.path().join("fixture")).unwrap();
            generate_fixture(arguments(root.path(), 'a'), None).unwrap();
            validate_signed_fixture(&root.path().join("fixture")).unwrap();
        }
    }

    #[test]
    fn verified_formal_directory_is_never_overwritten() {
        let root = tempdir().unwrap();
        generate_fixture(arguments(root.path(), 'a'), None).unwrap();
        let before = fs::read(root.path().join("fixture/catalog.json")).unwrap();

        let error = generate_fixture(arguments(root.path(), 'b'), None).unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            fs::read(root.path().join("fixture/catalog.json")).unwrap(),
            before
        );
    }

    #[test]
    fn incomplete_formal_directory_is_quarantined_before_publish() {
        let root = tempdir().unwrap();
        let output = root.path().join("fixture");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("partial.json"), b"partial").unwrap();

        generate_fixture(arguments(root.path(), 'a'), None).unwrap();
        validate_signed_fixture(&output).unwrap();
        assert!(fs::read_dir(root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .into_string()
                .expect("temporary fixture entry name must be UTF-8")
                .starts_with(".fixture.rejected-")
        }));
    }

    #[test]
    fn candidate_sha_must_already_be_canonical_lowercase() {
        let root = tempdir().unwrap();
        let error = generate_fixture(arguments(root.path(), 'A'), None).unwrap_err();
        assert!(error.to_string().contains("40 lowercase hexadecimal"));
        assert!(!root.path().join("fixture").exists());
    }
}
