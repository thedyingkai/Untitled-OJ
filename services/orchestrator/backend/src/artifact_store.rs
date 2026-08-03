use orchestrator_runtime::ArtifactReference;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub(crate) const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const DEFAULT_CHUNK_BYTES: u32 = 1024 * 1024;
pub(crate) const MAX_CHUNK_BYTES: u32 = 2 * 1024 * 1024;
const DEFAULT_RETENTION_DAYS: u64 = 30;
const DEFAULT_QUOTA_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const ABANDONED_UPLOAD_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Error)]
pub(crate) enum ArtifactStoreError {
    #[error("invalid artifact reference: {0}")]
    Invalid(String),
    #[error("artifact was not found")]
    NotFound,
    #[error("artifact storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artifact checksum or size does not match its durable reference")]
    Integrity,
}

#[derive(Debug, Clone)]
pub(crate) struct ArtifactStore {
    root: Arc<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct ArtifactChunk {
    pub bytes: Vec<u8>,
    pub offset: u64,
    pub total_size: u64,
    pub eof: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ArtifactRetentionPolicy {
    pub retention: Duration,
    pub quota_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArtifactGcReport {
    pub removed_files: u64,
    pub removed_bytes: u64,
    pub retained_bytes: u64,
}

impl ArtifactRetentionPolicy {
    pub(crate) fn from_env() -> Result<Self, ArtifactStoreError> {
        let days = std::env::var("ORCHESTRATOR_ARTIFACT_RETENTION_DAYS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|_| {
                ArtifactStoreError::Invalid(
                    "ORCHESTRATOR_ARTIFACT_RETENTION_DAYS must be an integer".to_string(),
                )
            })?
            .unwrap_or(DEFAULT_RETENTION_DAYS);
        let quota_bytes = std::env::var("ORCHESTRATOR_ARTIFACT_QUOTA_BYTES")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|_| {
                ArtifactStoreError::Invalid(
                    "ORCHESTRATOR_ARTIFACT_QUOTA_BYTES must be an integer".to_string(),
                )
            })?
            .unwrap_or(DEFAULT_QUOTA_BYTES);
        if days == 0 || days > 3650 || quota_bytes < MAX_ARTIFACT_BYTES {
            return Err(ArtifactStoreError::Invalid(format!(
                "artifact retention must be 1-3650 days and quota at least {MAX_ARTIFACT_BYTES} bytes"
            )));
        }
        Ok(Self {
            retention: Duration::from_secs(days.saturating_mul(24 * 60 * 60)),
            quota_bytes,
        })
    }
}

impl ArtifactStore {
    pub(crate) fn open(root: &Path) -> Result<Self, ArtifactStoreError> {
        let root = root.to_path_buf();
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        preflight_writable(&root)?;
        Ok(Self {
            root: Arc::new(root),
        })
    }

    pub(crate) fn create_oci_archive(
        &self,
        layout_root: &Path,
    ) -> Result<ArtifactReference, ArtifactStoreError> {
        let temporary = self.root.join(format!(
            ".upload-{}-{}.tar",
            std::process::id(),
            crate::api_v1::next_request_id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let mut builder = tar::Builder::new(file);
        builder.mode(tar::HeaderMode::Deterministic);
        builder.follow_symlinks(false);
        if let Err(error) = builder.append_dir_all(".", layout_root) {
            let _ = fs::remove_file(&temporary);
            return Err(ArtifactStoreError::Io(error));
        }
        if let Err(error) = builder.finish() {
            let _ = fs::remove_file(&temporary);
            return Err(ArtifactStoreError::Io(error));
        }
        let file = builder.into_inner()?;
        file.sync_all()?;
        let size_bytes = fs::metadata(&temporary)?.len();
        if size_bytes == 0 || size_bytes > MAX_ARTIFACT_BYTES {
            let _ = fs::remove_file(&temporary);
            return Err(ArtifactStoreError::Invalid(format!(
                "archive size must be between 1 and {MAX_ARTIFACT_BYTES} bytes"
            )));
        }
        let digest = hash_file(&temporary)?;
        let hex = digest.trim_start_matches("sha256:");
        let final_path = self.root.join(format!("{hex}.tar"));
        if final_path.exists() {
            let existing_size = fs::metadata(&final_path)?.len();
            if existing_size != size_bytes || hash_file(&final_path)? != digest {
                let _ = fs::remove_file(&temporary);
                return Err(ArtifactStoreError::Integrity);
            }
            fs::remove_file(&temporary)?;
        } else {
            fs::rename(&temporary, &final_path)?;
        }
        Ok(ArtifactReference {
            artifact_id: hex.to_string(),
            sha256: digest,
            size_bytes,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
        })
    }

    pub(crate) fn read_chunk(
        &self,
        reference: &ArtifactReference,
        offset: u64,
        requested_bytes: u32,
    ) -> Result<ArtifactChunk, ArtifactStoreError> {
        validate_reference(reference)?;
        if offset > reference.size_bytes {
            return Err(ArtifactStoreError::Invalid(
                "chunk offset exceeds artifact size".to_string(),
            ));
        }
        let requested_bytes = requested_bytes.clamp(1, MAX_CHUNK_BYTES);
        let path = self.root.join(format!("{}.tar", reference.artifact_id));
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ArtifactStoreError::NotFound);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.len() != reference.size_bytes {
            return Err(ArtifactStoreError::Integrity);
        }
        let remaining = reference.size_bytes.saturating_sub(offset);
        let length = remaining.min(u64::from(requested_bytes)) as usize;
        let mut bytes = vec![0_u8; length];
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut bytes)?;
        Ok(ArtifactChunk {
            bytes,
            offset,
            total_size: reference.size_bytes,
            eof: offset.saturating_add(length as u64) == reference.size_bytes,
        })
    }

    pub(crate) fn collect_garbage(
        &self,
        protected_artifact_ids: &BTreeSet<String>,
        policy: ArtifactRetentionPolicy,
        now: SystemTime,
    ) -> Result<ArtifactGcReport, ArtifactStoreError> {
        let mut total_bytes = 0_u64;
        let mut candidates = Vec::new();
        let mut report = ArtifactGcReport::default();
        for entry in fs::read_dir(self.root.as_ref())? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() {
                continue;
            }
            let name = entry.file_name().into_string().map_err(|_| {
                ArtifactStoreError::Invalid(
                    "artifact storage contains a non-UTF-8 file name".to_string(),
                )
            })?;
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let age = now.duration_since(modified).unwrap_or_default();
            if name.starts_with(".upload-") || name.starts_with(".write-probe-") {
                if age >= ABANDONED_UPLOAD_TTL {
                    fs::remove_file(&path)?;
                    report.removed_files += 1;
                    report.removed_bytes = report.removed_bytes.saturating_add(metadata.len());
                }
                continue;
            }
            let Some(artifact_id) = name.strip_suffix(".tar").filter(|id| valid_artifact_id(id))
            else {
                continue;
            };
            total_bytes = total_bytes.checked_add(metadata.len()).ok_or_else(|| {
                ArtifactStoreError::Invalid("artifact storage byte count overflow".to_string())
            })?;
            if !protected_artifact_ids.contains(artifact_id) {
                candidates.push((modified, path, metadata.len()));
            }
        }
        candidates.sort_by_key(|(modified, _, _)| *modified);
        for (modified, path, size) in candidates {
            let expired = now.duration_since(modified).unwrap_or_default() >= policy.retention;
            if !expired && total_bytes <= policy.quota_bytes {
                continue;
            }
            fs::remove_file(path)?;
            total_bytes = total_bytes.saturating_sub(size);
            report.removed_files += 1;
            report.removed_bytes = report.removed_bytes.saturating_add(size);
        }
        report.retained_bytes = total_bytes;
        Ok(report)
    }
}

fn validate_reference(reference: &ArtifactReference) -> Result<(), ArtifactStoreError> {
    let valid_id = valid_artifact_id(&reference.artifact_id);
    if !valid_id
        || reference.sha256 != format!("sha256:{}", reference.artifact_id)
        || reference.size_bytes == 0
        || reference.size_bytes > MAX_ARTIFACT_BYTES
        || reference.chunk_bytes == 0
        || reference.chunk_bytes > MAX_CHUNK_BYTES
    {
        return Err(ArtifactStoreError::Invalid(
            "artifact id, checksum, size, or chunk bound is invalid".to_string(),
        ));
    }
    Ok(())
}

fn valid_artifact_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn preflight_writable(root: &Path) -> Result<(), ArtifactStoreError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = root.join(format!(".write-probe-{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.write_all(b"orchestrator-artifact-preflight")?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    fs::remove_file(path)?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, ArtifactStoreError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_artifact_root_is_independent_of_read_only_resources() {
        let resources = tempfile::tempdir().unwrap();
        let artifact_parent = tempfile::tempdir().unwrap();
        let resource_file = resources.path().join("installed-resource.txt");
        fs::write(&resource_file, b"immutable").unwrap();
        let original_permissions = fs::metadata(resources.path()).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(resources.path(), permissions.clone()).unwrap();

        let artifact_root = artifact_parent.path().join("artifacts");
        let store = ArtifactStore::open(&artifact_root).unwrap();
        assert!(store.root.is_dir());
        assert_eq!(fs::read(resource_file).unwrap(), b"immutable");

        let _ = fs::set_permissions(resources.path(), original_permissions);
    }

    #[cfg(unix)]
    #[test]
    fn open_rejects_existing_read_only_artifact_directory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let result = ArtifactStore::open(directory.path());
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn retention_deletes_unprotected_artifacts_and_keeps_active_ones() {
        let directory = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(directory.path()).unwrap();
        let protected_id = "a".repeat(64);
        let expired_id = "b".repeat(64);
        fs::write(
            directory.path().join(format!("{protected_id}.tar")),
            b"active",
        )
        .unwrap();
        fs::write(
            directory.path().join(format!("{expired_id}.tar")),
            b"expired",
        )
        .unwrap();
        let report = store
            .collect_garbage(
                &BTreeSet::from([protected_id.clone()]),
                ArtifactRetentionPolicy {
                    retention: Duration::ZERO,
                    quota_bytes: u64::MAX,
                },
                SystemTime::now(),
            )
            .unwrap();
        assert_eq!(report.removed_files, 1);
        assert!(
            directory
                .path()
                .join(format!("{protected_id}.tar"))
                .is_file()
        );
        assert!(!directory.path().join(format!("{expired_id}.tar")).exists());
    }
}
