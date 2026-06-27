use crate::manifest::{validate_manifest, validate_package_entry_path};
use crate::{InstallerError, Manifest, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageVerification {
    pub valid: bool,
    pub module_id: String,
    pub version: String,
    pub files_checked: usize,
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub fn package_module(module_dir: &Path, output: &Path) -> Result<PackageVerification> {
    let manifest_path = module_dir.join("module.yaml");
    let manifest_text = fs::read_to_string(&manifest_path)?;
    let manifest: Manifest = serde_yaml::from_str(&manifest_text)?;
    validate_manifest(&manifest)?;

    let mut entries = Vec::new();
    for entry in WalkDir::new(module_dir).follow_links(false) {
        let entry = entry.map_err(|err| InstallerError::Package(err.to_string()))?;
        if entry.file_type().is_dir() {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(InstallerError::Package(
                "symlink is not allowed in module package".to_string(),
            ));
        }
        let rel = entry
            .path()
            .strip_prefix(module_dir)
            .map_err(|_| InstallerError::Package("module file is outside module_dir".to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        validate_package_entry_name(&rel)?;
        if rel == "checksums.sha256" {
            continue;
        }
        entries.push((rel, entry.path().to_path_buf()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut checksums = String::new();
    for (rel, path) in &entries {
        let hash = sha256_file(path)?;
        checksums.push_str(&format!("{}  {}\n", hash, rel));
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().unix_permissions(0o644);
    for (rel, path) in &entries {
        zip.start_file(rel, options)?;
        let mut file = File::open(path)?;
        std::io::copy(&mut file, &mut zip)?;
    }
    zip.start_file("checksums.sha256", options)?;
    zip.write_all(checksums.as_bytes())?;
    zip.finish()?;

    verify_package(output)
}

pub fn verify_package(package_path: &Path) -> Result<PackageVerification> {
    let file = File::open(package_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut names = HashSet::new();
    let mut manifest_text = None;
    let mut checksum_text = None;
    let mut actual = HashMap::new();

    for idx in 0..archive.len() {
        let mut file = archive.by_index(idx)?;
        let name = file.name().replace('\\', "/");
        validate_package_entry_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(InstallerError::Package(
                "duplicate package entry".to_string(),
            ));
        }
        if file.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        let hash = sha256_bytes(&data);
        actual.insert(name.clone(), hash);
        if name == "module.yaml" {
            manifest_text =
                Some(String::from_utf8(data).map_err(|_| {
                    InstallerError::Package("module.yaml must be utf-8".to_string())
                })?);
        } else if name == "checksums.sha256" {
            checksum_text = Some(String::from_utf8(data).map_err(|_| {
                InstallerError::Package("checksums.sha256 must be utf-8".to_string())
            })?);
        }
    }

    let manifest_text = manifest_text
        .ok_or_else(|| InstallerError::Package("module.yaml is missing".to_string()))?;
    let checksum_text = checksum_text
        .ok_or_else(|| InstallerError::Package("checksums.sha256 is missing".to_string()))?;
    let manifest: Manifest = serde_yaml::from_str(&manifest_text)?;
    validate_manifest(&manifest)?;

    let expected = parse_checksums(&checksum_text)?;
    if expected.is_empty() {
        return Err(InstallerError::Package(
            "checksums.sha256 is empty".to_string(),
        ));
    }
    if !expected.contains_key("module.yaml") {
        return Err(InstallerError::Package(
            "checksums.sha256 must include module.yaml".to_string(),
        ));
    }
    for (name, want) in &expected {
        validate_package_entry_name(name)?;
        let got = actual.get(name).ok_or_else(|| {
            InstallerError::Package(format!("checksummed file {} is missing", name))
        })?;
        if got != want {
            return Err(InstallerError::Package(format!(
                "checksum mismatch for {}",
                name
            )));
        }
    }
    for name in actual.keys() {
        if name != "checksums.sha256" && !expected.contains_key(name) {
            return Err(InstallerError::Package(format!(
                "file {} is not checksummed",
                name
            )));
        }
    }

    Ok(PackageVerification {
        valid: true,
        module_id: manifest.id,
        version: manifest.version,
        files_checked: expected.len(),
        warnings: vec![
            "v0 verifies checksum integrity only; signature trust policy is reserved for v1"
                .to_string(),
        ],
    })
}

fn validate_package_entry_name(name: &str) -> Result<()> {
    validate_package_entry_path(name)?;
    let lower = name.to_ascii_lowercase();
    let banned_exact = [".env", ".env.local"];
    let banned_segments = [".tmp", "node_modules", "frontend/dist", ".git", "target"];
    let banned_hooks = [
        "postinstall",
        "preinstall",
        "hook",
        "script",
        ".ps1",
        ".bat",
        ".cmd",
        ".exe",
    ];
    if banned_exact.iter().any(|item| lower == *item) {
        return Err(InstallerError::Package(format!(
            "banned package entry {}",
            name
        )));
    }
    if banned_segments.iter().any(|item| lower.contains(item)) {
        return Err(InstallerError::Package(format!(
            "banned package path {}",
            name
        )));
    }
    if banned_hooks.iter().any(|item| lower.contains(item)) {
        return Err(InstallerError::Package(format!(
            "executable hook is not allowed: {}",
            name
        )));
    }
    Ok(())
}

fn parse_checksums(text: &str) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) || name.is_empty() {
            return Err(InstallerError::Package(format!(
                "invalid checksum line {}",
                line_no + 1
            )));
        }
        if parts.next().is_some() {
            return Err(InstallerError::Package(format!(
                "invalid checksum line {}",
                line_no + 1
            )));
        }
        out.insert(name.to_string(), hash.to_ascii_lowercase());
    }
    Ok(out)
}

fn sha256_file(path: &PathBuf) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}
