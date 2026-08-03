use crate::NodeCertificateBundle;
use fs2::FileExt;
use rcgen::{
    CertificateParams, CertificateSigningRequestParams, DistinguishedName, DnType, KeyPair,
    PublicKeyData,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use x509_parser::extensions::GeneralName;
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

const IDENTITY_SCHEMA_VERSION: u32 = 1;
const ENROLLMENT_REQUEST_SCHEMA_VERSION: u32 = 1;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("Node identity storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Node identity metadata is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Node identity is invalid: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedCertificateRequest {
    pub csr_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentAttempt {
    Pending(GeneratedCertificateRequest),
    Completed {
        csr_pem: String,
        installed_serial: String,
    },
}

impl EnrollmentAttempt {
    pub fn csr_pem(&self) -> &str {
        match self {
            Self::Pending(request) => &request.csr_pem,
            Self::Completed { csr_pem, .. } => csr_pem,
        }
    }

    pub fn private_key_pem(&self) -> Option<&str> {
        match self {
            Self::Pending(request) => Some(&request.private_key_pem),
            Self::Completed { .. } => None,
        }
    }

    pub fn installed_serial(&self) -> Option<&str> {
        match self {
            Self::Pending(_) => None,
            Self::Completed {
                installed_serial, ..
            } => Some(installed_serial),
        }
    }
}

#[derive(Debug)]
pub struct EnrollmentSessionGuard {
    _file: fs::File,
}

#[derive(Debug, Clone)]
pub struct IdentityStore {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct StoredNodeIdentity {
    pub node_id: String,
    pub spiffe_id: String,
    pub serial_hex: String,
    pub not_after_ms: i64,
    pub renew_after_ms: i64,
    pub generation: String,
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
    pub server_ca_path: PathBuf,
    pub node_ca_path: PathBuf,
}

impl StoredNodeIdentity {
    /// Validates a recovered generation against an optional provisioning
    /// expectation. When the caller did not know the Node ID before the first
    /// enrollment, the exact CSR/serial-selected generation remains the source
    /// of truth and still has its own Node/SPIFFE binding verified below.
    pub fn validate_recovery_binding_for(
        &self,
        expected_node_id: Option<&str>,
        expected_server_ca_pem: &[u8],
    ) -> Result<(), IdentityError> {
        self.validate_recovery_binding(
            expected_node_id.unwrap_or(self.node_id.as_str()),
            expected_server_ca_pem,
        )
    }

    /// Proves that durable metadata and the certificate URI SAN describe the
    /// exact Node expected by the provisioning caller.
    pub fn validate_recovery_binding(
        &self,
        expected_node_id: &str,
        expected_server_ca_pem: &[u8],
    ) -> Result<(), IdentityError> {
        let expected_node_id = expected_node_id.trim();
        let expected_spiffe_id = format!("spiffe://ojos.local/node/{expected_node_id}");
        if expected_node_id.is_empty()
            || expected_node_id.contains('/')
            || self.node_id != expected_node_id
            || self.spiffe_id != expected_spiffe_id
            || self.serial_hex.trim().is_empty()
        {
            return Err(IdentityError::Invalid(format!(
                "recoverable identity {} does not match expected Node {expected_node_id}",
                self.node_id
            )));
        }

        let certificate_pem = fs::read(&self.certificate_path)?;
        let (remainder, pem) = parse_x509_pem(&certificate_pem).map_err(|_| {
            IdentityError::Invalid(format!(
                "recoverable identity certificate {} is not valid PEM",
                self.certificate_path.display()
            ))
        })?;
        if pem.label != "CERTIFICATE" || remainder.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err(IdentityError::Invalid(
                "recoverable identity certificate must contain exactly one PEM certificate"
                    .to_string(),
            ));
        }
        let (_, certificate) = parse_x509_certificate(&pem.contents).map_err(|_| {
            IdentityError::Invalid(
                "recoverable identity certificate is not valid X.509".to_string(),
            )
        })?;
        let san = certificate
            .subject_alternative_name()
            .map_err(|_| {
                IdentityError::Invalid(
                    "recoverable identity certificate has an invalid subjectAltName".to_string(),
                )
            })?
            .ok_or_else(|| {
                IdentityError::Invalid(
                    "recoverable identity certificate has no subjectAltName".to_string(),
                )
            })?;
        let spiffe_ids = san
            .value
            .general_names
            .iter()
            .filter_map(|name| match name {
                GeneralName::URI(uri) if uri.starts_with("spiffe://ojos.local/node/") => Some(*uri),
                _ => None,
            })
            .collect::<Vec<_>>();
        if spiffe_ids != [expected_spiffe_id.as_str()]
            || normalize_serial(&certificate.raw_serial_as_string())
                != self.serial_hex.to_ascii_lowercase()
        {
            return Err(IdentityError::Invalid(
                "recoverable certificate SPIFFE URI or serial does not match its durable metadata"
                    .to_string(),
            ));
        }
        if expected_server_ca_pem.is_empty()
            || fs::read(&self.server_ca_path)? != expected_server_ca_pem
        {
            return Err(IdentityError::Invalid(
                "recoverable identity is bound to a different control-plane CA".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct IdentityMetadata {
    schema_version: u32,
    node_id: String,
    spiffe_id: String,
    serial_hex: String,
    not_after_ms: i64,
    renew_after_ms: i64,
    installed_at_ms: i64,
}

#[derive(Debug, Serialize)]
struct CurrentGeneration<'a> {
    schema_version: u32,
    generation: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EnrollmentRequestMetadata {
    schema_version: u32,
    control_plane: String,
    expected_node_id: Option<String>,
    enrollment_code_sha256: String,
    server_ca_sha256: String,
    csr_sha256: String,
    created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    installed_serial: Option<String>,
}

pub fn generate_certificate_request() -> Result<GeneratedCertificateRequest, IdentityError> {
    let key = KeyPair::generate()
        .map_err(|error| IdentityError::Invalid(format!("generate private key: {error}")))?;
    let mut params = CertificateParams::default();
    let mut subject = DistinguishedName::new();
    subject.push(DnType::CommonName, "OJOS Orchestrator Node");
    params.distinguished_name = subject;
    let csr_pem = params
        .serialize_request(&key)
        .map_err(|error| IdentityError::Invalid(format!("generate CSR: {error}")))?
        .pem()
        .map_err(|error| IdentityError::Invalid(format!("encode CSR: {error}")))?;
    Ok(GeneratedCertificateRequest {
        csr_pem,
        private_key_pem: key.serialize_pem(),
    })
}

/// Rejects a committed enrollment replay that has aged past its certificate
/// lifetime before any generation is installed. A spent one-time code must not
/// turn an expired historical response into an `ENROLLED` success.
pub fn validate_enrollment_bundle_fresh(
    bundle: &NodeCertificateBundle,
    now_ms: i64,
) -> Result<(), IdentityError> {
    if now_ms <= 0 || bundle.not_after_ms <= now_ms {
        return Err(IdentityError::Invalid(format!(
            "enrollment certificate {} expired at {}",
            bundle.serial_hex, bundle.not_after_ms
        )));
    }
    Ok(())
}

impl IdentityStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Verifies the identity directory is writable before a one-time code is
    /// redeemed or the current certificate is rotated.
    pub fn preflight(&self) -> Result<(), IdentityError> {
        if self.root.exists() && !self.root.is_dir() {
            return Err(IdentityError::Invalid(format!(
                "{} is not a directory",
                self.root.display()
            )));
        }
        create_dir_all_durable(&self.generations_dir())?;
        // Persist every directory entry that protects the enrollment state.
        // File fsync alone does not make a newly created directory or a rename
        // durable across a Linux host crash.
        if let Some(parent) = self
            .root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            sync_directory(parent)?;
        }
        sync_directory(&self.root)?;
        sync_directory(&self.generations_dir())?;
        let probe = self.root.join(format!(
            ".write-probe-{}-{}-{}",
            std::process::id(),
            crate::now_ms(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&probe)?;
        file.write_all(b"identity-store-preflight")?;
        file.sync_all()?;
        fs::remove_file(probe)?;
        sync_directory(&self.root)?;
        Ok(())
    }

    /// Serializes the complete enrollment state machine, including its HTTP
    /// request. The shorter `.identity.lock` still protects individual local
    /// mutations; this distinct lock prevents two Agent processes from
    /// interleaving prepare/redeem/install/complete sequences.
    pub fn begin_enrollment_session(&self) -> Result<EnrollmentSessionGuard, IdentityError> {
        self.preflight()?;
        let file = open_lock_file(&self.root.join(".enrollment-session.lock"))?;
        sync_directory(&self.root)?;
        file.lock_exclusive()?;
        Ok(EnrollmentSessionGuard { _file: file })
    }

    /// Returns the durable state for this exact enrollment code. A pending
    /// attempt retains its private key so a lost HTTP response can replay the
    /// exact CSR. A completed marker intentionally contains no private key and
    /// binds retries to the exact installed certificate serial.
    pub fn prepare_enrollment_attempt(
        &self,
        control_plane: &str,
        expected_node_id: Option<&str>,
        enrollment_code: &str,
        server_ca_pem: &[u8],
    ) -> Result<EnrollmentAttempt, IdentityError> {
        self.preflight()?;
        let _lock = self.lock_identity_store()?;
        self.cleanup_staging_directories(".pending-enrollment-")?;
        self.cleanup_staging_directories(".completed-enrollment-")?;
        let expected = enrollment_binding(
            control_plane,
            expected_node_id,
            enrollment_code,
            server_ca_pem,
        )?;
        let completed_directory = self.completed_enrollment_dir(&expected)?;
        if completed_directory.exists() {
            return load_completed_enrollment(&completed_directory, &expected);
        }

        let final_directory = self.pending_enrollment_dir();
        if final_directory.exists() {
            let pending_metadata = load_enrollment_metadata(&final_directory)?;
            if pending_metadata.enrollment_code_sha256 == expected.enrollment_code_sha256 {
                // The same one-time code must retain every original bootstrap
                // binding and the byte-identical CSR. A changed CA, origin, or
                // expected Node is therefore still a hard conflict.
                return load_enrollment_request(&final_directory, &expected)
                    .map(EnrollmentAttempt::Pending);
            }
            // A replacement code must not destroy the original CSR/private
            // key. The old server redemption may already have committed even
            // when its response was lost. Archive the complete attempt so a
            // later retry of that exact code can still replay byte-for-byte.
            self.archive_pending_enrollment(&final_directory, &pending_metadata)?;
        }

        let archived_directory = self.pending_enrollment_archive_dir(&expected)?;
        if archived_directory.exists() {
            let request = load_enrollment_request(&archived_directory, &expected)?;
            fs::rename(&archived_directory, &final_directory)?;
            sync_directory(&self.root)?;
            sync_directory(&self.pending_enrollment_archive_root())?;
            return Ok(EnrollmentAttempt::Pending(request));
        }

        let request = generate_certificate_request()?;
        validate_certificate_request(&request)?;
        let pending = self.root.join(format!(
            ".pending-enrollment-{}-{}-{}",
            std::process::id(),
            crate::now_ms(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&pending)?;
        write_secret(
            &pending.join("private-key.pem"),
            request.private_key_pem.as_bytes(),
        )?;
        write_synced(&pending.join("request.pem"), request.csr_pem.as_bytes())?;
        let metadata = EnrollmentRequestMetadata {
            csr_sha256: sha256(request.csr_pem.as_bytes()),
            created_at_ms: crate::now_ms(),
            installed_serial: None,
            ..expected
        };
        write_synced(
            &pending.join("request.json"),
            &serde_json::to_vec_pretty(&metadata)?,
        )?;
        sync_directory(&pending)?;
        fs::rename(&pending, &final_directory)?;
        sync_directory(&self.root)?;
        load_enrollment_request(&final_directory, &metadata).map(EnrollmentAttempt::Pending)
    }

    /// Publishes a no-private-key completion marker before erasing the pending
    /// enrollment key. A retry with the same code can therefore recover the
    /// exact installed generation without attempting a second redemption.
    pub fn complete_enrollment_attempt(
        &self,
        attempt: &EnrollmentAttempt,
        installed_serial: &str,
    ) -> Result<(), IdentityError> {
        self.preflight()?;
        let _lock = self.lock_identity_store()?;
        self.cleanup_staging_directories(".completed-enrollment-")?;
        let installed_serial = validate_serial(installed_serial)?;

        let pending_directory = self.pending_enrollment_dir();
        let (request, mut metadata) = match attempt {
            EnrollmentAttempt::Pending(request) => {
                let metadata = load_enrollment_metadata(&pending_directory)?;
                let persisted = load_enrollment_request(&pending_directory, &metadata)?;
                if &persisted != request {
                    return Err(IdentityError::Invalid(
                        "pending enrollment request changed before completion".to_string(),
                    ));
                }
                (persisted, metadata)
            }
            EnrollmentAttempt::Completed {
                csr_pem,
                installed_serial: completed_serial,
            } => {
                if validate_serial(completed_serial)? != installed_serial {
                    return Err(IdentityError::Invalid(
                        "completed enrollment serial does not match the installed identity"
                            .to_string(),
                    ));
                }
                let identity = load_generation(&self.generations_dir().join(&installed_serial))?;
                if !certificate_matches_csr(&identity, csr_pem)? {
                    return Err(IdentityError::Invalid(
                        "completed enrollment marker does not match the installed identity"
                            .to_string(),
                    ));
                }
                self.cleanup_pending_enrollment_attempts()?;
                return Ok(());
            }
        };

        let identity = load_generation(&self.generations_dir().join(&installed_serial))?;
        if identity.serial_hex.to_ascii_lowercase() != installed_serial
            || !certificate_matches_request(&identity, &request)?
        {
            return Err(IdentityError::Invalid(
                "installed identity does not match the pending enrollment CSR and serial"
                    .to_string(),
            ));
        }
        metadata.installed_serial = Some(installed_serial.clone());
        let completed_parent = self.completed_enrollment_root();
        create_dir_all_durable(&completed_parent)?;
        let final_directory = self.completed_enrollment_dir(&metadata)?;
        if final_directory.exists() {
            let completed = load_completed_enrollment(&final_directory, &metadata)?;
            if completed.csr_pem() != request.csr_pem
                || completed.installed_serial() != Some(installed_serial.as_str())
            {
                return Err(IdentityError::Invalid(
                    "completed enrollment marker conflicts with the installed identity".to_string(),
                ));
            }
        } else {
            let staging = self.root.join(format!(
                ".completed-enrollment-{}-{}-{}",
                std::process::id(),
                crate::now_ms(),
                TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&staging)?;
            write_synced(&staging.join("request.pem"), request.csr_pem.as_bytes())?;
            write_synced(
                &staging.join("request.json"),
                &serde_json::to_vec_pretty(&metadata)?,
            )?;
            sync_directory(&staging)?;
            sync_directory(&self.root)?;
            fs::rename(&staging, &final_directory)?;
            sync_directory(&self.root)?;
            sync_directory(&completed_parent)?;
        }

        // The marker is durable first. Losing power after this point cannot
        // lose both the replay proof and the pending private key.
        self.cleanup_pending_enrollment_attempts()?;
        self.cleanup_staging_directories(".pending-enrollment-")?;
        self.cleanup_staging_directories(".pending-generation-")?;
        Ok(())
    }

    pub fn install(
        &self,
        bundle: &NodeCertificateBundle,
        private_key_pem: &str,
        server_ca_pem: &[u8],
    ) -> Result<StoredNodeIdentity, IdentityError> {
        self.install_with_post_publish_hook(bundle, private_key_pem, server_ca_pem, || Ok(()))
    }

    /// Durably publishes an immutable certificate generation without changing
    /// the current pointer. Enrollment uses this boundary so the certificate
    /// can prove its exact server-side mTLS binding before it becomes current.
    pub fn install_unpublished(
        &self,
        bundle: &NodeCertificateBundle,
        private_key_pem: &str,
        server_ca_pem: &[u8],
    ) -> Result<StoredNodeIdentity, IdentityError> {
        self.install_generation(bundle, private_key_pem, server_ca_pem, || Ok(()), false)
    }

    fn install_with_post_publish_hook<F>(
        &self,
        bundle: &NodeCertificateBundle,
        private_key_pem: &str,
        server_ca_pem: &[u8],
        after_generation_publish: F,
    ) -> Result<StoredNodeIdentity, IdentityError>
    where
        F: FnOnce() -> Result<(), IdentityError>,
    {
        self.install_generation(
            bundle,
            private_key_pem,
            server_ca_pem,
            after_generation_publish,
            true,
        )
    }

    fn install_generation<F>(
        &self,
        bundle: &NodeCertificateBundle,
        private_key_pem: &str,
        server_ca_pem: &[u8],
        after_generation_publish: F,
        publish_current: bool,
    ) -> Result<StoredNodeIdentity, IdentityError>
    where
        F: FnOnce() -> Result<(), IdentityError>,
    {
        self.preflight()?;
        let _lock = self.lock_identity_store()?;
        self.cleanup_staging_directories(".pending-generation-")?;
        validate_bundle(bundle, private_key_pem, server_ca_pem)?;

        let generation = bundle.serial_hex.to_ascii_lowercase();
        let final_directory = self.generations_dir().join(&generation);
        if !final_directory.exists() {
            // Stage outside `generations/`. A crash during a partial write is
            // therefore never mistaken for a published generation; initial
            // enrollment can replay its durable CSR and renewal can retry with
            // the still-active old certificate.
            let pending = self.root.join(format!(
                ".pending-generation-{generation}-{}-{}-{}",
                std::process::id(),
                crate::now_ms(),
                TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&pending)?;
            write_secret(&pending.join("private-key.pem"), private_key_pem.as_bytes())?;
            write_synced(
                &pending.join("certificate.pem"),
                bundle.certificate_pem.as_bytes(),
            )?;
            write_synced(
                &pending.join("node-ca.pem"),
                bundle.ca_certificate_pem.as_bytes(),
            )?;
            write_synced(&pending.join("server-ca.pem"), server_ca_pem)?;
            let metadata = IdentityMetadata {
                schema_version: IDENTITY_SCHEMA_VERSION,
                node_id: bundle.node_id.clone(),
                spiffe_id: bundle.spiffe_id.clone(),
                serial_hex: generation.clone(),
                not_after_ms: bundle.not_after_ms,
                renew_after_ms: bundle.renew_after_ms,
                installed_at_ms: crate::now_ms(),
            };
            write_synced(
                &pending.join("identity.json"),
                &serde_json::to_vec_pretty(&metadata)?,
            )?;
            sync_directory(&pending)?;
            // Persist the source directory entry before the cross-directory
            // rename, then persist both the removal and destination entry.
            sync_directory(&self.root)?;
            // A complete pending directory remains recoverable if the process
            // dies after the server has revoked the previous certificate.
            fs::rename(&pending, &final_directory)?;
            sync_directory(&self.root)?;
            sync_directory(&self.generations_dir())?;
        } else {
            let existing = load_generation(&final_directory)?;
            if existing.serial_hex != generation
                || existing.node_id != bundle.node_id
                || existing.spiffe_id != bundle.spiffe_id
                || existing.not_after_ms != bundle.not_after_ms
            {
                return Err(IdentityError::Invalid(format!(
                    "identity generation {generation} already exists with different metadata"
                )));
            }
        }

        // This is the deliberate crash boundary exercised by tests. A caller
        // that is interrupted here has consumed the one-time enrollment code,
        // but the complete generation is durable and can be recovered without
        // redeeming that code again.
        after_generation_publish()?;
        let identity = load_generation(&final_directory)?;
        if publish_current {
            self.write_current(&generation)?;
        }
        Ok(identity)
    }

    /// Recovers a complete enrollment generation before a one-time code is
    /// redeemed. Every generation entry must be complete and well-formed; a
    /// partial or ambiguously named identity fails closed instead of being
    /// hidden by an older generation. The caller-provided validator runs before
    /// any pointer mutation. The selected generation is returned unpublished;
    /// the caller must prove its live mTLS binding and then explicitly call
    /// `publish_recovered_identity`.
    pub fn recover_enrollment_identity<F>(
        &self,
        enrollment_attempt: Option<&EnrollmentAttempt>,
        validate: F,
    ) -> Result<Option<StoredNodeIdentity>, IdentityError>
    where
        F: Fn(&StoredNodeIdentity) -> Result<(), IdentityError>,
    {
        self.preflight()?;
        let _lock = self.lock_identity_store()?;
        let current_exists = self.root.join("current.json").exists();
        let entries = match fs::read_dir(self.generations_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !current_exists => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let mut identities = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Err(IdentityError::Invalid(format!(
                    "unexpected non-directory identity generation {}",
                    entry.path().display()
                )));
            }
            let identity = load_generation(&entry.path()).map_err(|error| {
                IdentityError::Invalid(format!(
                    "identity generation {} is partial or invalid: {error}",
                    entry.path().display()
                ))
            })?;
            let name = entry.file_name().into_string().map_err(|_| {
                IdentityError::Invalid(format!(
                    "identity generation path {} is not valid UTF-8",
                    entry.path().display()
                ))
            })?;
            let serial = identity.serial_hex.to_ascii_lowercase();
            if name != serial {
                return Err(IdentityError::Invalid(format!(
                    "identity generation directory {name} does not match serial {serial}"
                )));
            }
            identities.push(identity);
        }
        if identities.is_empty() {
            if current_exists {
                return Err(IdentityError::Invalid(
                    "current identity pointer exists without a complete generation".to_string(),
                ));
            }
            return Ok(None);
        }
        if let Some(attempt) = enrollment_attempt {
            if let EnrollmentAttempt::Pending(request) = attempt {
                validate_certificate_request(request)?;
            }
            let expected_serial = attempt
                .installed_serial()
                .map(validate_serial)
                .transpose()?;
            identities = identities
                .into_iter()
                .filter_map(|identity| {
                    if expected_serial
                        .as_ref()
                        .is_some_and(|serial| identity.serial_hex.to_ascii_lowercase() != *serial)
                    {
                        return None;
                    }
                    match certificate_matches_csr(&identity, attempt.csr_pem()) {
                        Ok(true) => Some(Ok(identity)),
                        Ok(false) => None,
                        Err(error) => Some(Err(error)),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            if identities.is_empty() {
                // An enrollment attempt denotes a specific CSR (and, once
                // completed, a specific serial). Older generations are never
                // evidence that this one-time redemption was durably stored.
                return Ok(None);
            }
        }
        identities.sort_by(|left, right| {
            left.not_after_ms
                .cmp(&right.not_after_ms)
                .then_with(|| left.serial_hex.cmp(&right.serial_hex))
        });
        let identity = identities.pop().expect("non-empty checked");
        validate(&identity)?;
        Ok(Some(identity))
    }

    /// Atomically publishes the exact generation previously selected and
    /// validated by recovery. Reloading it under the store lock prevents a
    /// caller from publishing a different serial after the online proof.
    pub fn publish_recovered_identity(
        &self,
        recovered: &StoredNodeIdentity,
    ) -> Result<StoredNodeIdentity, IdentityError> {
        self.preflight()?;
        let _lock = self.lock_identity_store()?;
        let serial = validate_serial(&recovered.serial_hex)?;
        let persisted = load_generation(&self.generations_dir().join(&serial))?;
        if persisted.node_id != recovered.node_id
            || persisted.spiffe_id != recovered.spiffe_id
            || persisted.serial_hex.to_ascii_lowercase() != serial
            || persisted.not_after_ms != recovered.not_after_ms
            || persisted.renew_after_ms != recovered.renew_after_ms
            || persisted.certificate_path != recovered.certificate_path
            || persisted.private_key_path != recovered.private_key_path
            || persisted.server_ca_path != recovered.server_ca_path
            || persisted.node_ca_path != recovered.node_ca_path
        {
            return Err(IdentityError::Invalid(
                "recovered identity generation changed before publication".to_string(),
            ));
        }
        let mut newest = persisted.clone();
        for entry in fs::read_dir(self.generations_dir())? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Err(IdentityError::Invalid(format!(
                    "unexpected non-directory identity generation {}",
                    entry.path().display()
                )));
            }
            let candidate = load_generation(&entry.path()).map_err(|error| {
                IdentityError::Invalid(format!(
                    "identity generation {} is partial or invalid: {error}",
                    entry.path().display()
                ))
            })?;
            if (candidate.not_after_ms, candidate.serial_hex.as_str())
                > (newest.not_after_ms, newest.serial_hex.as_str())
            {
                newest = candidate;
            }
        }
        if newest.serial_hex.to_ascii_lowercase() != serial {
            return Err(IdentityError::Invalid(format!(
                "refusing to replace newer current identity generation {} with recovered generation {serial}",
                newest.serial_hex
            )));
        }
        self.write_current(&serial)?;
        Ok(persisted)
    }

    /// Loads the newest complete generation. `current.json` is intentionally a
    /// hint rather than the source of truth: scanning complete generations
    /// recovers a rotation interrupted after the old certificate was revoked.
    pub fn load(&self) -> Result<StoredNodeIdentity, IdentityError> {
        let directory = self.generations_dir();
        let entries = fs::read_dir(&directory).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                IdentityError::Invalid(format!(
                    "no enrolled Node identity exists in {}",
                    self.root.display()
                ))
            } else {
                IdentityError::Io(error)
            }
        })?;
        let mut identities = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Ok(identity) = load_generation(&entry.path()) {
                identities.push(identity);
            }
        }
        identities.sort_by(|left, right| {
            left.not_after_ms
                .cmp(&right.not_after_ms)
                .then_with(|| left.serial_hex.cmp(&right.serial_hex))
        });
        identities.pop().ok_or_else(|| {
            IdentityError::Invalid(format!(
                "no complete Node identity generation exists in {}",
                directory.display()
            ))
        })
    }

    fn generations_dir(&self) -> PathBuf {
        self.root.join("generations")
    }

    fn pending_enrollment_dir(&self) -> PathBuf {
        self.root.join("pending-enrollment")
    }

    fn completed_enrollment_root(&self) -> PathBuf {
        self.root.join("completed-enrollment")
    }

    fn pending_enrollment_archive_root(&self) -> PathBuf {
        self.root.join("pending-enrollment-archive")
    }

    fn pending_enrollment_archive_dir(
        &self,
        metadata: &EnrollmentRequestMetadata,
    ) -> Result<PathBuf, IdentityError> {
        Ok(self
            .pending_enrollment_archive_root()
            .join(digest_component(&metadata.enrollment_code_sha256)?))
    }

    fn archive_pending_enrollment(
        &self,
        pending_directory: &Path,
        metadata: &EnrollmentRequestMetadata,
    ) -> Result<(), IdentityError> {
        let request = load_enrollment_request(pending_directory, metadata)?;
        let parent = self.pending_enrollment_archive_root();
        create_dir_all_durable(&parent)?;
        let final_directory = self.pending_enrollment_archive_dir(metadata)?;
        if final_directory.exists() {
            let existing = load_enrollment_request(&final_directory, metadata)?;
            if existing != request {
                return Err(IdentityError::Invalid(
                    "archived enrollment attempt conflicts with its original CSR or private key"
                        .to_string(),
                ));
            }
            fs::remove_dir_all(pending_directory)?;
            sync_directory(&self.root)?;
            return Ok(());
        }
        sync_directory(&self.root)?;
        sync_directory(&parent)?;
        fs::rename(pending_directory, &final_directory)?;
        sync_directory(&self.root)?;
        sync_directory(&parent)?;
        Ok(())
    }

    fn cleanup_pending_enrollment_attempts(&self) -> Result<(), IdentityError> {
        let pending = self.pending_enrollment_dir();
        if pending.exists() {
            fs::remove_dir_all(&pending)?;
            sync_directory(&self.root)?;
        }
        let archive = self.pending_enrollment_archive_root();
        if archive.exists() {
            fs::remove_dir_all(&archive)?;
            sync_directory(&self.root)?;
        }
        Ok(())
    }

    fn completed_enrollment_dir(
        &self,
        metadata: &EnrollmentRequestMetadata,
    ) -> Result<PathBuf, IdentityError> {
        Ok(self
            .completed_enrollment_root()
            .join(digest_component(&metadata.enrollment_code_sha256)?))
    }

    fn lock_identity_store(&self) -> Result<fs::File, IdentityError> {
        let path = self.root.join(".identity.lock");
        let file = open_lock_file(&path)?;
        sync_directory(&self.root)?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn cleanup_staging_directories(&self, prefix: &str) -> Result<(), IdentityError> {
        let mut changed = false;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                IdentityError::Invalid(format!(
                    "identity staging path {} is not valid UTF-8",
                    entry.path().display()
                ))
            })?;
            if !name.starts_with(prefix) {
                continue;
            }
            let file_type = entry.file_type()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                return Err(IdentityError::Invalid(format!(
                    "identity staging path {} is not a real directory",
                    entry.path().display()
                )));
            }
            fs::remove_dir_all(entry.path())?;
            changed = true;
        }
        if changed {
            sync_directory(&self.root)?;
        }
        Ok(())
    }

    fn write_current(&self, generation: &str) -> Result<(), IdentityError> {
        let temporary = self.root.join(format!(
            ".current-{}-{}.tmp",
            std::process::id(),
            crate::now_ms()
        ));
        let contents = serde_json::to_vec_pretty(&CurrentGeneration {
            schema_version: IDENTITY_SCHEMA_VERSION,
            generation,
        })?;
        write_synced(&temporary, &contents)?;
        let current = self.root.join("current.json");
        match fs::rename(&temporary, &current) {
            Ok(()) => {}
            Err(error)
                if current.exists()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
                    ) =>
            {
                // Windows does not replace an existing file with rename. A
                // missing pointer is safe because load() scans generations.
                fs::remove_file(&current)?;
                fs::rename(&temporary, &current)?;
            }
            Err(error) => return Err(error.into()),
        }
        sync_directory(&self.root)
    }
}

fn enrollment_binding(
    control_plane: &str,
    expected_node_id: Option<&str>,
    enrollment_code: &str,
    server_ca_pem: &[u8],
) -> Result<EnrollmentRequestMetadata, IdentityError> {
    let control_plane = control_plane.trim();
    let expected_node_id = expected_node_id.map(str::trim);
    if control_plane.is_empty()
        || enrollment_code.is_empty()
        || server_ca_pem.is_empty()
        || expected_node_id.is_some_and(|node_id| node_id.is_empty() || node_id.contains('/'))
    {
        return Err(IdentityError::Invalid(
            "enrollment request binding is incomplete or invalid".to_string(),
        ));
    }
    Ok(EnrollmentRequestMetadata {
        schema_version: ENROLLMENT_REQUEST_SCHEMA_VERSION,
        control_plane: control_plane.to_string(),
        expected_node_id: expected_node_id.map(str::to_string),
        enrollment_code_sha256: sha256(enrollment_code.as_bytes()),
        server_ca_sha256: sha256(server_ca_pem),
        csr_sha256: String::new(),
        created_at_ms: 0,
        installed_serial: None,
    })
}

fn load_enrollment_metadata(directory: &Path) -> Result<EnrollmentRequestMetadata, IdentityError> {
    if !directory.is_dir() {
        return Err(IdentityError::Invalid(format!(
            "enrollment marker {} is not a directory",
            directory.display()
        )));
    }
    let metadata: EnrollmentRequestMetadata =
        serde_json::from_slice(&fs::read(directory.join("request.json"))?)?;
    if metadata.schema_version != ENROLLMENT_REQUEST_SCHEMA_VERSION
        || metadata.control_plane.trim().is_empty()
        || metadata.enrollment_code_sha256 != sha256_digest(&metadata.enrollment_code_sha256)?
        || metadata.server_ca_sha256 != sha256_digest(&metadata.server_ca_sha256)?
        || metadata.csr_sha256 != sha256_digest(&metadata.csr_sha256)?
        || metadata.created_at_ms <= 0
        || metadata
            .expected_node_id
            .as_deref()
            .is_some_and(|node_id| node_id.trim().is_empty() || node_id.contains('/'))
    {
        return Err(IdentityError::Invalid(format!(
            "enrollment marker metadata in {} is invalid",
            directory.display()
        )));
    }
    if let Some(serial) = metadata.installed_serial.as_deref() {
        validate_serial(serial)?;
    }
    Ok(metadata)
}

fn enrollment_binding_matches(
    actual: &EnrollmentRequestMetadata,
    expected: &EnrollmentRequestMetadata,
) -> bool {
    actual.control_plane == expected.control_plane
        && actual.expected_node_id == expected.expected_node_id
        && actual.enrollment_code_sha256 == expected.enrollment_code_sha256
        && actual.server_ca_sha256 == expected.server_ca_sha256
}

fn load_enrollment_request(
    directory: &Path,
    expected: &EnrollmentRequestMetadata,
) -> Result<GeneratedCertificateRequest, IdentityError> {
    let metadata = load_enrollment_metadata(directory)?;
    if !enrollment_binding_matches(&metadata, expected) || metadata.installed_serial.is_some() {
        return Err(IdentityError::Invalid(
            "pending enrollment request is bound to different bootstrap inputs".to_string(),
        ));
    }
    let request = GeneratedCertificateRequest {
        csr_pem: fs::read_to_string(directory.join("request.pem"))?,
        private_key_pem: fs::read_to_string(directory.join("private-key.pem"))?,
    };
    if metadata.csr_sha256 != sha256(request.csr_pem.as_bytes()) {
        return Err(IdentityError::Invalid(
            "pending enrollment CSR digest does not match its metadata".to_string(),
        ));
    }
    validate_certificate_request(&request)?;
    Ok(request)
}

fn load_completed_enrollment(
    directory: &Path,
    expected: &EnrollmentRequestMetadata,
) -> Result<EnrollmentAttempt, IdentityError> {
    let metadata = load_enrollment_metadata(directory)?;
    if !enrollment_binding_matches(&metadata, expected) {
        return Err(IdentityError::Invalid(
            "completed enrollment marker is bound to different bootstrap inputs".to_string(),
        ));
    }
    let installed_serial = metadata.installed_serial.clone().ok_or_else(|| {
        IdentityError::Invalid("completed enrollment marker has no installed serial".to_string())
    })?;
    if directory.join("private-key.pem").exists() {
        return Err(IdentityError::Invalid(
            "completed enrollment marker must not retain a private key".to_string(),
        ));
    }
    if directory.file_name().and_then(|name| name.to_str())
        != Some(digest_component(&metadata.enrollment_code_sha256)?)
    {
        return Err(IdentityError::Invalid(
            "completed enrollment marker path does not match its enrollment code digest"
                .to_string(),
        ));
    }
    let csr_pem = fs::read_to_string(directory.join("request.pem"))?;
    if metadata.csr_sha256 != sha256(csr_pem.as_bytes()) {
        return Err(IdentityError::Invalid(
            "completed enrollment CSR digest does not match its metadata".to_string(),
        ));
    }
    validate_csr(&csr_pem)?;
    Ok(EnrollmentAttempt::Completed {
        csr_pem,
        installed_serial,
    })
}

fn validate_certificate_request(
    request: &GeneratedCertificateRequest,
) -> Result<(), IdentityError> {
    let csr = validate_csr(&request.csr_pem)?;
    let key = KeyPair::from_pem(&request.private_key_pem).map_err(|error| {
        IdentityError::Invalid(format!("parse enrollment private key: {error}"))
    })?;
    if csr.public_key.subject_public_key_info() != key.subject_public_key_info() {
        return Err(IdentityError::Invalid(
            "pending enrollment CSR does not match its private key".to_string(),
        ));
    }
    Ok(())
}

fn validate_csr(csr_pem: &str) -> Result<CertificateSigningRequestParams, IdentityError> {
    CertificateSigningRequestParams::from_pem(csr_pem)
        .map_err(|error| IdentityError::Invalid(format!("parse enrollment CSR: {error}")))
}

fn certificate_matches_request(
    identity: &StoredNodeIdentity,
    request: &GeneratedCertificateRequest,
) -> Result<bool, IdentityError> {
    validate_certificate_request(request)?;
    certificate_matches_csr(identity, &request.csr_pem)
}

fn certificate_matches_csr(
    identity: &StoredNodeIdentity,
    csr_pem: &str,
) -> Result<bool, IdentityError> {
    let certificate_pem = fs::read(&identity.certificate_path)?;
    let (remainder, pem) = parse_x509_pem(&certificate_pem).map_err(|_| {
        IdentityError::Invalid(format!(
            "identity certificate {} is not valid PEM",
            identity.certificate_path.display()
        ))
    })?;
    if pem.label != "CERTIFICATE" || remainder.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(IdentityError::Invalid(
            "identity certificate must contain exactly one PEM certificate".to_string(),
        ));
    }
    let (_, certificate) = parse_x509_certificate(&pem.contents).map_err(|_| {
        IdentityError::Invalid("identity certificate is not valid X.509".to_string())
    })?;
    let csr = validate_csr(csr_pem)?;
    Ok(certificate.public_key().raw == csr.public_key.subject_public_key_info())
}

fn validate_bundle(
    bundle: &NodeCertificateBundle,
    private_key_pem: &str,
    server_ca_pem: &[u8],
) -> Result<(), IdentityError> {
    if bundle.node_id.trim().is_empty()
        || bundle.node_id.contains('/')
        || bundle.spiffe_id != format!("spiffe://ojos.local/node/{}", bundle.node_id)
    {
        return Err(IdentityError::Invalid(
            "certificate response contains an invalid Node identity".to_string(),
        ));
    }
    if bundle.serial_hex.is_empty()
        || bundle.serial_hex.len() > 128
        || !bundle
            .serial_hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(IdentityError::Invalid(
            "certificate serial must be hexadecimal".to_string(),
        ));
    }
    if bundle.renew_after_ms <= 0
        || bundle.not_after_ms <= bundle.renew_after_ms
        || bundle.certificate_pem.trim().is_empty()
        || bundle.ca_certificate_pem.trim().is_empty()
        || private_key_pem.trim().is_empty()
        || server_ca_pem.is_empty()
    {
        return Err(IdentityError::Invalid(
            "certificate response or key material is incomplete".to_string(),
        ));
    }
    Ok(())
}

fn load_generation(directory: &Path) -> Result<StoredNodeIdentity, IdentityError> {
    let metadata: IdentityMetadata =
        serde_json::from_slice(&fs::read(directory.join("identity.json"))?)?;
    if metadata.schema_version != IDENTITY_SCHEMA_VERSION
        || metadata.node_id.trim().is_empty()
        || metadata.serial_hex.is_empty()
        || metadata.renew_after_ms <= 0
        || metadata.not_after_ms <= metadata.renew_after_ms
    {
        return Err(IdentityError::Invalid(format!(
            "invalid identity metadata in {}",
            directory.display()
        )));
    }
    let certificate_path = directory.join("certificate.pem");
    let private_key_path = directory.join("private-key.pem");
    let server_ca_path = directory.join("server-ca.pem");
    let node_ca_path = directory.join("node-ca.pem");
    for path in [
        &certificate_path,
        &private_key_path,
        &server_ca_path,
        &node_ca_path,
    ] {
        if fs::metadata(path)?.len() == 0 {
            return Err(IdentityError::Invalid(format!(
                "identity material {} is empty",
                path.display()
            )));
        }
    }
    Ok(StoredNodeIdentity {
        node_id: metadata.node_id,
        spiffe_id: metadata.spiffe_id,
        serial_hex: metadata.serial_hex,
        not_after_ms: metadata.not_after_ms,
        renew_after_ms: metadata.renew_after_ms,
        generation: directory
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        certificate_path,
        private_key_path,
        server_ca_path,
        node_ca_path,
    })
}

fn normalize_serial(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect()
}

fn validate_serial(value: &str) -> Result<String, IdentityError> {
    let serial = value.trim().to_ascii_lowercase();
    if serial.is_empty()
        || serial.len() > 128
        || !serial.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(IdentityError::Invalid(
            "certificate serial must be canonical hexadecimal".to_string(),
        ));
    }
    Ok(serial)
}

fn sha256(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn sha256_digest(value: &str) -> Result<String, IdentityError> {
    let digest = value.strip_prefix("sha256:").ok_or_else(|| {
        IdentityError::Invalid("enrollment digest must use the sha256: prefix".to_string())
    })?;
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || digest.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        return Err(IdentityError::Invalid(
            "enrollment digest must be 64 lowercase hexadecimal characters".to_string(),
        ));
    }
    Ok(format!("sha256:{digest}"))
}

fn digest_component(value: &str) -> Result<&str, IdentityError> {
    let canonical = sha256_digest(value)?;
    // Validation above guarantees the borrowed input has this exact prefix and
    // canonical payload; return a slice of the original to avoid allocation in
    // path construction.
    debug_assert_eq!(canonical, value);
    Ok(value
        .strip_prefix("sha256:")
        .expect("validated sha256 prefix"))
}

fn write_synced(path: &Path, contents: &[u8]) -> Result<(), IdentityError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

/// Creates every missing path component separately and persists each parent
/// entry before moving deeper. `create_dir_all` alone provides no crash
/// durability guarantee for the intermediate directory entries.
fn create_dir_all_durable(path: &Path) -> Result<(), IdentityError> {
    if path.is_dir() {
        return Ok(());
    }
    if path.exists() {
        return Err(IdentityError::Invalid(format!(
            "{} exists but is not a directory",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent != Path::new(".") || !parent.is_dir() {
        create_dir_all_durable(parent)?;
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error.into()),
    }
    sync_directory(parent)
}

#[cfg(unix)]
fn open_lock_file(path: &Path) -> Result<fs::File, IdentityError> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> Result<fs::File, IdentityError> {
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), IdentityError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_directory(path: &Path) -> Result<(), IdentityError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FlushFileBuffers, OPEN_EXISTING,
    };

    let canonical = fs::canonicalize(path)?;
    let wide = canonical
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: `wide` is NUL-terminated and remains alive for the call. The
    // returned handle is checked and closed on every successful open path.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `handle` is a valid directory handle opened with GENERIC_WRITE.
    let flush_result = unsafe { FlushFileBuffers(handle) };
    let flush_error = (flush_result == 0).then(std::io::Error::last_os_error);
    // SAFETY: this function owns `handle` and closes it exactly once.
    let close_result = unsafe { CloseHandle(handle) };
    if let Some(error) = flush_error {
        return Err(error.into());
    }
    if close_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_path: &Path) -> Result<(), IdentityError> {
    Ok(())
}

#[cfg(unix)]
fn write_secret(path: &Path, contents: &[u8]) -> Result<(), IdentityError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret(path: &Path, contents: &[u8]) -> Result<(), IdentityError> {
    write_synced(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, SanType, SerialNumber};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    fn bundle(serial: &str, not_after_ms: i64) -> NodeCertificateBundle {
        NodeCertificateBundle {
            node_id: "node-1".to_string(),
            spiffe_id: "spiffe://ojos.local/node/node-1".to_string(),
            serial_hex: serial.to_string(),
            certificate_pem: "-----BEGIN CERTIFICATE-----\nleaf\n-----END CERTIFICATE-----\n"
                .to_string(),
            ca_certificate_pem: "-----BEGIN CERTIFICATE-----\nnode-ca\n-----END CERTIFICATE-----\n"
                .to_string(),
            not_after_ms,
            renew_after_ms: not_after_ms - 100,
        }
    }

    fn cryptographic_bundle(node_id: &str, serial: u8) -> (NodeCertificateBundle, String, Vec<u8>) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = vec![SanType::URI(
            format!("spiffe://ojos.local/node/{node_id}")
                .try_into()
                .unwrap(),
        )];
        params.serial_number = Some(SerialNumber::from_slice(&[serial]));
        let certificate = params.self_signed(&key).unwrap();
        let certificate_pem = certificate.pem();
        let serial_hex = format!("{serial:02x}");
        (
            NodeCertificateBundle {
                node_id: node_id.to_string(),
                spiffe_id: format!("spiffe://ojos.local/node/{node_id}"),
                serial_hex,
                certificate_pem: certificate_pem.clone(),
                ca_certificate_pem: certificate_pem.clone(),
                not_after_ms: 4_000_000_000_000,
                renew_after_ms: 3_000_000_000_000,
            },
            key.serialize_pem(),
            certificate_pem.into_bytes(),
        )
    }

    fn bundle_for_request(
        node_id: &str,
        serial: u8,
        request: &GeneratedCertificateRequest,
    ) -> NodeCertificateBundle {
        let key = KeyPair::from_pem(&request.private_key_pem).unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = vec![SanType::URI(
            format!("spiffe://ojos.local/node/{node_id}")
                .try_into()
                .unwrap(),
        )];
        params.serial_number = Some(SerialNumber::from_slice(&[serial]));
        let certificate_pem = params.self_signed(&key).unwrap().pem();
        NodeCertificateBundle {
            node_id: node_id.to_string(),
            spiffe_id: format!("spiffe://ojos.local/node/{node_id}"),
            serial_hex: format!("{serial:02x}"),
            certificate_pem: certificate_pem.clone(),
            ca_certificate_pem: certificate_pem,
            not_after_ms: 4_000_000_000_000,
            renew_after_ms: 3_000_000_000_000,
        }
    }

    #[test]
    fn install_and_load_use_the_newest_complete_generation() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        store
            .install(&bundle("01", 1_000), "private-key-1", b"server-ca")
            .unwrap();
        store
            .install(&bundle("02", 2_000), "private-key-2", b"server-ca")
            .unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.serial_hex, "02");
        assert_eq!(loaded.generation, "02");
        assert_eq!(
            fs::read_to_string(loaded.private_key_path).unwrap(),
            "private-key-2"
        );
    }

    #[test]
    fn generation_scan_recovers_when_current_pointer_is_missing() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        store
            .install(&bundle("0a", 5_000), "private-key", b"server-ca")
            .unwrap();
        fs::remove_file(directory.path().join("current.json")).unwrap();
        assert_eq!(store.load().unwrap().serial_hex, "0a");
    }

    #[test]
    fn enrollment_recovers_exact_generation_rename_before_current_boundary() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let (bundle, private_key, server_ca) = cryptographic_bundle("node-1", 0x0a);

        let error = store
            .install_with_post_publish_hook(&bundle, &private_key, &server_ca, || {
                Err(IdentityError::Invalid(
                    "injected crash after generation rename".to_string(),
                ))
            })
            .unwrap_err();
        assert!(error.to_string().contains("injected crash"));
        assert!(directory.path().join("generations/0a").is_dir());
        assert!(!directory.path().join("current.json").exists());

        let recovered = store
            .recover_enrollment_identity(None, |identity| {
                identity.validate_recovery_binding("node-1", &server_ca)
            })
            .unwrap()
            .expect("complete generation must be recoverable");
        assert_eq!(recovered.node_id, "node-1");
        assert_eq!(recovered.generation, "0a");
        assert!(
            !directory.path().join("current.json").exists(),
            "local recovery selection must not publish before an online mTLS proof"
        );
        store.publish_recovered_identity(&recovered).unwrap();
        let current: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("current.json")).unwrap())
                .unwrap();
        assert_eq!(current["generation"], "0a");
    }

    #[test]
    fn enrollment_request_survives_lost_response_and_reuses_exact_csr_and_key() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let ca = b"test-control-plane-ca";
        let first = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "one-time-code",
                ca,
            )
            .unwrap();

        // Model a new Agent process after the control plane committed the
        // certificate but its HTTP response was lost. The retry must use the
        // same CSR so the server can return that exact certificate.
        let restarted = IdentityStore::new(directory.path());
        let replay = restarted
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "one-time-code",
                ca,
            )
            .unwrap();
        assert_eq!(replay, first);

        for (control_plane, node_id, server_ca) in [
            ("https://other.example", Some("node-1"), ca.as_slice()),
            ("https://control.example", Some("node-2"), ca.as_slice()),
            (
                "https://control.example",
                Some("node-1"),
                b"different-ca".as_slice(),
            ),
        ] {
            assert!(
                restarted
                    .prepare_enrollment_attempt(control_plane, node_id, "one-time-code", server_ca,)
                    .is_err(),
                "a durable CSR must not cross its bootstrap binding"
            );
        }

        // A different code may be tried and rejected by the control plane
        // after code A was already committed but its response was lost. The
        // exact A key must remain recoverable instead of being tombstoned.
        let replacement = restarted
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "different-code",
                ca,
            )
            .unwrap();
        assert_ne!(replacement, first);
        let committed_replay = restarted
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "one-time-code",
                ca,
            )
            .unwrap();
        assert_eq!(committed_replay, first);
        assert!(directory.path().join("pending-enrollment").is_dir());
        assert!(directory.path().join("pending-enrollment-archive").is_dir());
    }

    #[test]
    fn completed_marker_recovers_exact_attempt_and_new_code_forces_reenrollment() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let server_ca = b"stable-control-plane-ca";
        let attempt = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "first-code",
                server_ca,
            )
            .unwrap();
        let request = match &attempt {
            EnrollmentAttempt::Pending(request) => request,
            EnrollmentAttempt::Completed { .. } => panic!("new enrollment must be pending"),
        };
        let bundle = bundle_for_request("node-1", 0x21, request);
        store
            .install(&bundle, &request.private_key_pem, server_ca)
            .unwrap();
        store
            .complete_enrollment_attempt(&attempt, &bundle.serial_hex)
            .unwrap();

        assert!(!directory.path().join("pending-enrollment").exists());
        let marker = fs::read_dir(directory.path().join("completed-enrollment"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(marker.join("request.json").is_file());
        assert!(marker.join("request.pem").is_file());
        assert!(!marker.join("private-key.pem").exists());

        let completed = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "first-code",
                server_ca,
            )
            .unwrap();
        assert!(matches!(
            &completed,
            EnrollmentAttempt::Completed {
                installed_serial,
                ..
            } if installed_serial == "21"
        ));
        let recovered = store
            .recover_enrollment_identity(Some(&completed), |identity| {
                identity.validate_recovery_binding("node-1", server_ca)
            })
            .unwrap()
            .expect("completed marker must recover its exact generation");
        assert_eq!(recovered.serial_hex, "21");

        let replacement = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "replacement-code",
                server_ca,
            )
            .unwrap();
        assert!(matches!(replacement, EnrollmentAttempt::Pending(_)));
        assert!(
            store
                .recover_enrollment_identity(Some(&replacement), |identity| {
                    identity.validate_recovery_binding("node-1", server_ca)
                })
                .unwrap()
                .is_none(),
            "an old current generation must not short-circuit a new code"
        );
    }

    #[test]
    fn accepted_old_recovery_cannot_replace_a_newer_current_generation() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let server_ca = b"stable-control-plane-ca";
        let attempt = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "old-code",
                server_ca,
            )
            .unwrap();
        let EnrollmentAttempt::Pending(request) = &attempt else {
            panic!("new enrollment must be pending")
        };
        let old_bundle = bundle_for_request("node-1", 0x21, request);
        store
            .install(&old_bundle, &request.private_key_pem, server_ca)
            .unwrap();
        store
            .complete_enrollment_attempt(&attempt, &old_bundle.serial_hex)
            .unwrap();
        let completed = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "old-code",
                server_ca,
            )
            .unwrap();

        let (new_bundle, new_key, _) = cryptographic_bundle("node-1", 0x22);
        store.install(&new_bundle, &new_key, server_ca).unwrap();
        let selected_old = store
            .recover_enrollment_identity(Some(&completed), |identity| {
                identity.validate_recovery_binding("node-1", server_ca)
            })
            .unwrap()
            .expect("the completed marker selects its exact historical generation");
        assert_eq!(selected_old.serial_hex, "21");

        // Even if the read-only server probe still accepts the old serial,
        // publication is monotonic and cannot roll current back from 22.
        let error = store.publish_recovered_identity(&selected_old).unwrap_err();
        assert!(error.to_string().contains("newer current identity"));
        let current: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.path().join("current.json")).unwrap())
                .unwrap();
        assert_eq!(current["generation"], "22");
    }

    #[test]
    fn a_new_code_replaces_an_unredeemed_pending_attempt_with_a_new_csr() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let first = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "expired-code-a",
                b"server-ca",
            )
            .unwrap();
        let EnrollmentAttempt::Pending(first) = first else {
            panic!("the first code must create a pending attempt")
        };

        let second = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "replacement-code-b",
                b"server-ca",
            )
            .unwrap();
        let EnrollmentAttempt::Pending(second) = second else {
            panic!("the replacement code must create a pending attempt")
        };
        assert_ne!(second.csr_pem, first.csr_pem);
        assert_ne!(second.private_key_pem, first.private_key_pem);

        let replay = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "replacement-code-b",
                b"server-ca",
            )
            .unwrap();
        assert_eq!(replay, EnrollmentAttempt::Pending(second));

        let restored_first = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "expired-code-a",
                b"server-ca",
            )
            .unwrap();
        assert_eq!(restored_first, EnrollmentAttempt::Pending(first));

        // Completing B only after its live proof (performed by the CLI caller)
        // erases every now-obsolete archived private key.
        let restored_second = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "replacement-code-b",
                b"server-ca",
            )
            .unwrap();
        let EnrollmentAttempt::Pending(restored_second_request) = &restored_second else {
            panic!("the replacement attempt must remain pending")
        };
        let replacement_bundle = bundle_for_request("node-1", 0x31, restored_second_request);
        store
            .install(
                &replacement_bundle,
                &restored_second_request.private_key_pem,
                b"server-ca",
            )
            .unwrap();
        store
            .complete_enrollment_attempt(&restored_second, &replacement_bundle.serial_hex)
            .unwrap();
        assert!(!directory.path().join("pending-enrollment").exists());
        assert!(!directory.path().join("pending-enrollment-archive").exists());
    }

    #[test]
    fn enrollment_session_lock_spans_the_entire_process_sequence() {
        let directory = tempdir().unwrap();
        let store = Arc::new(IdentityStore::new(directory.path()));
        let first_session = store.begin_enrollment_session().unwrap();
        let (sender, receiver) = mpsc::channel();
        let contender = Arc::clone(&store);
        let handle = thread::spawn(move || {
            sender.send("waiting").unwrap();
            let _second_session = contender.begin_enrollment_session().unwrap();
            sender.send("acquired").unwrap();
        });
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "waiting"
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(first_session);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            "acquired"
        );
        handle.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_flushes_each_created_directory_and_a_rename_destination() {
        let directory = tempdir().unwrap();
        let nested = directory.path().join("one").join("two").join("three");
        create_dir_all_durable(&nested).unwrap();
        let staging = nested.join("staging");
        write_synced(&staging, b"durable").unwrap();
        let published = nested.join("published");
        fs::rename(&staging, &published).unwrap();
        sync_directory(&nested).unwrap();
        sync_directory(nested.parent().unwrap()).unwrap();
        assert_eq!(fs::read(published).unwrap(), b"durable");
    }

    #[test]
    fn enrollment_request_lock_cleans_abandoned_staging_and_converges_concurrently() {
        let directory = tempdir().unwrap();
        let store = Arc::new(IdentityStore::new(directory.path()));
        store.preflight().unwrap();
        let abandoned = directory.path().join(".pending-enrollment-crashed");
        fs::create_dir(&abandoned).unwrap();
        fs::write(abandoned.join("private-key.pem"), b"abandoned-secret").unwrap();

        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .prepare_enrollment_attempt(
                            "https://control.example",
                            Some("node-1"),
                            "one-time-code",
                            b"test-control-plane-ca",
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let requests = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(requests.iter().all(|request| request == &requests[0]));
        let abandoned = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .expect("temporary identity test paths are valid UTF-8")
                    .starts_with(".pending-enrollment-")
            })
            .count();
        assert_eq!(abandoned, 0, "private-key staging must not survive");
    }

    #[test]
    fn pending_csr_never_recovers_an_unrelated_old_generation() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let server_ca = b"stable-control-plane-ca";
        let (old_bundle, old_key, _) = cryptographic_bundle("node-1", 0x0c);
        store.install(&old_bundle, &old_key, server_ca).unwrap();
        let attempt = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "replacement-code",
                server_ca,
            )
            .unwrap();
        let request = match &attempt {
            EnrollmentAttempt::Pending(request) => request,
            EnrollmentAttempt::Completed { .. } => panic!("new code must be pending"),
        };
        fs::remove_file(directory.path().join("current.json")).unwrap();

        let recovered = store
            .recover_enrollment_identity(Some(&attempt), |identity| {
                identity.validate_recovery_binding("node-1", server_ca)
            })
            .unwrap();
        assert!(recovered.is_none());
        assert!(!directory.path().join("current.json").exists());

        let replacement = bundle_for_request("node-1", 0x0d, &request);
        store
            .install(&replacement, &request.private_key_pem, server_ca)
            .unwrap();
        fs::remove_file(directory.path().join("current.json")).unwrap();
        let recovered = store
            .recover_enrollment_identity(Some(&attempt), |identity| {
                identity.validate_recovery_binding("node-1", server_ca)
            })
            .unwrap()
            .expect("matching CSR generation is recoverable");
        assert_eq!(recovered.serial_hex, "0d");
    }

    #[test]
    fn enrollment_request_rejects_a_tampered_private_key_before_redeem() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let ca = b"test-control-plane-ca";
        store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "one-time-code",
                ca,
            )
            .unwrap();
        let replacement = generate_certificate_request().unwrap();
        fs::write(
            directory.path().join("pending-enrollment/private-key.pem"),
            replacement.private_key_pem,
        )
        .unwrap();
        let error = store
            .prepare_enrollment_attempt(
                "https://control.example",
                Some("node-1"),
                "one-time-code",
                ca,
            )
            .unwrap_err();
        assert!(error.to_string().contains("does not match its private key"));
    }

    #[test]
    fn enrollment_recovery_rejects_wrong_node_ca_and_partial_generation() {
        for failure in ["node", "ca", "partial"] {
            let directory = tempdir().unwrap();
            let store = IdentityStore::new(directory.path());
            let (bundle, private_key, server_ca) = cryptographic_bundle("node-1", 0x0b);
            let _ = store.install_with_post_publish_hook(&bundle, &private_key, &server_ca, || {
                Err(IdentityError::Invalid("injected crash".to_string()))
            });
            if failure == "partial" {
                fs::remove_file(directory.path().join("generations/0b/certificate.pem")).unwrap();
            }
            let result = store.recover_enrollment_identity(None, |identity| match failure {
                "node" => identity.validate_recovery_binding("node-2", &server_ca),
                "ca" => identity.validate_recovery_binding("node-1", b"different-ca"),
                "partial" => identity.validate_recovery_binding("node-1", &server_ca),
                _ => unreachable!(),
            });
            assert!(result.is_err(), "{failure} identity must fail closed");
            assert!(!directory.path().join("current.json").exists());
        }
    }

    #[test]
    fn enrollment_recovery_without_an_expected_node_uses_the_exact_generation_binding() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let (bundle, private_key, server_ca) = cryptographic_bundle("node-1", 0x1b);
        let identity = store.install(&bundle, &private_key, &server_ca).unwrap();

        identity
            .validate_recovery_binding_for(None, &server_ca)
            .unwrap();
        identity
            .validate_recovery_binding_for(Some("node-1"), &server_ca)
            .unwrap();
        assert!(
            identity
                .validate_recovery_binding_for(Some("node-2"), &server_ca)
                .is_err()
        );
    }

    #[test]
    fn invalid_spiffe_identity_is_rejected_before_writing() {
        let directory = tempdir().unwrap();
        let store = IdentityStore::new(directory.path());
        let mut invalid = bundle("0b", 5_000);
        invalid.spiffe_id = "spiffe://attacker.invalid/node/node-1".to_string();
        assert!(
            store
                .install(&invalid, "private-key", b"server-ca")
                .is_err()
        );
    }

    #[test]
    fn csr_generation_returns_a_pem_key_and_request() {
        let request = generate_certificate_request().unwrap();
        assert!(request.csr_pem.contains("BEGIN CERTIFICATE REQUEST"));
        assert!(request.private_key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn spent_code_replay_cannot_install_an_expired_certificate() {
        let response = bundle("2a", 5_000);
        validate_enrollment_bundle_fresh(&response, 4_999).unwrap();
        let error = validate_enrollment_bundle_fresh(&response, 5_000).unwrap_err();
        assert!(error.to_string().contains("expired at 5000"));
        assert!(validate_enrollment_bundle_fresh(&response, 5_001).is_err());
    }
}
