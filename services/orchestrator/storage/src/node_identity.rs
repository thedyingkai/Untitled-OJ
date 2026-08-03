use crate::{
    PostgresError, PostgresOrchestratorStore, SqliteOrchestratorStore, StorageError, sqlite::NODES,
};
use orchestrator_legacy::{NodeRecord, validate_node_record};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub const CERTIFICATE_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
pub const CERTIFICATE_RENEWAL_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub const MAX_REMOTE_NODES: usize = 100;
const NODE_ENROLLMENT_CAPACITY_LOCK_KEY: i64 = i64::from_be_bytes(*b"OJOSNODE");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeEnrollmentCode {
    pub code_id: String,
    pub secret_sha256: String,
    pub node_id: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub redeemed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NewNodeCertificate {
    pub serial_hex: String,
    pub node_id: String,
    pub spiffe_id: String,
    pub certificate_pem: String,
    pub fingerprint_sha256: String,
    pub issued_at_ms: i64,
    pub not_before_ms: i64,
    pub not_after_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeCertificateRecord {
    pub serial_hex: String,
    pub node_id: String,
    pub spiffe_id: String,
    pub certificate_pem: String,
    pub fingerprint_sha256: String,
    pub issued_at_ms: i64,
    pub not_before_ms: i64,
    pub not_after_ms: i64,
    pub revoked_at_ms: Option<i64>,
    pub revoke_reason: Option<String>,
    pub replaced_by_serial: Option<String>,
}

impl From<NewNodeCertificate> for NodeCertificateRecord {
    fn from(value: NewNodeCertificate) -> Self {
        Self {
            serial_hex: value.serial_hex,
            node_id: value.node_id,
            spiffe_id: value.spiffe_id,
            certificate_pem: value.certificate_pem,
            fingerprint_sha256: value.fingerprint_sha256,
            issued_at_ms: value.issued_at_ms,
            not_before_ms: value.not_before_ms,
            not_after_ms: value.not_after_ms,
            revoked_at_ms: None,
            revoke_reason: None,
            replaced_by_serial: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentRedemption {
    Redeemed(Box<NodeCertificateRecord>),
    Replayed(Box<NodeCertificateRecord>),
    ReplayCertificateRevoked,
    ReplayCertificateNotYetValid,
    ReplayCertificateExpired,
    NotFound,
    Expired,
    AlreadyRedeemed,
    NodeMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentLookup {
    Pending(NodeEnrollmentCode),
    Replayed(Box<NodeCertificateRecord>),
    NotFound,
    AlreadyRedeemed,
}

/// Applies the same certificate-liveness decision to both the pre-signer
/// replay lookup and the transactional redemption race loser. Keeping this in
/// storage prevents a concurrent redeem/revoke interleaving from returning a
/// revoked certificate after the advisory lookup already saw Pending.
pub fn classify_enrollment_replay(
    certificate: Box<NodeCertificateRecord>,
    now_ms: i64,
) -> EnrollmentRedemption {
    if certificate.revoked_at_ms.is_some() {
        EnrollmentRedemption::ReplayCertificateRevoked
    } else if now_ms < certificate.not_before_ms {
        EnrollmentRedemption::ReplayCertificateNotYetValid
    } else if now_ms >= certificate.not_after_ms {
        EnrollmentRedemption::ReplayCertificateExpired
    } else {
        EnrollmentRedemption::Replayed(certificate)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateRotation {
    Rotated(NodeCertificateRecord),
    NotFound,
    Revoked,
    Expired,
    NotDue { renew_at_ms: i64 },
    NodeMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateActivation {
    Activated { revoked_certificates: u64 },
    NotFound,
    Revoked,
    Expired,
    Superseded,
    NodeMismatch,
}

fn validate_code(code: &NodeEnrollmentCode) -> Result<(), String> {
    if code.code_id.trim().is_empty()
        || code.secret_sha256.trim().is_empty()
        || code.node_id.trim().is_empty()
    {
        return Err("enrollment code id, digest, and node id are required".to_string());
    }
    if code.created_at_ms < 0 || code.expires_at_ms <= code.created_at_ms {
        return Err("enrollment code expiry must be after creation".to_string());
    }
    Ok(())
}

fn validate_certificate(certificate: &NewNodeCertificate) -> Result<(), String> {
    if certificate.serial_hex.trim().is_empty()
        || certificate.node_id.trim().is_empty()
        || certificate.spiffe_id.trim().is_empty()
        || certificate.certificate_pem.trim().is_empty()
        || certificate.fingerprint_sha256.trim().is_empty()
    {
        return Err("certificate serial, node, SPIFFE id, and PEM are required".to_string());
    }
    if certificate.not_after_ms <= certificate.not_before_ms
        || certificate.issued_at_ms < certificate.not_before_ms
        || certificate.not_after_ms - certificate.not_before_ms > CERTIFICATE_LIFETIME_MS
    {
        return Err("node certificate validity must be at most 30 days".to_string());
    }
    let expected = format!("spiffe://ojos.local/node/{}", certificate.node_id);
    if certificate.spiffe_id != expected {
        return Err("node certificate SPIFFE id does not match node id".to_string());
    }
    Ok(())
}

fn validate_csr_sha256(value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err("enrollment CSR digest must use sha256:<64 hex>".to_string());
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("enrollment CSR digest must use sha256:<64 hex>".to_string());
    }
    Ok(())
}

impl SqliteOrchestratorStore {
    pub fn register_node_enrollment(
        &self,
        node: &NodeRecord,
        code: &NodeEnrollmentCode,
    ) -> Result<(), StorageError> {
        validate_code(code).map_err(StorageError::Invariant)?;
        validate_node_record(node).map_err(|error| StorageError::Domain(error.to_string()))?;
        if code.node_id != node.node_id || !node.status.eq_ignore_ascii_case("ENROLLMENT_PENDING") {
            return Err(StorageError::Invariant(
                "enrollment code and pending Node record must identify the same Node".into(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let nodes = {
            let mut statement = transaction.prepare(
                "SELECT payload FROM orchestrator_records WHERE kind = ?1 ORDER BY record_key",
            )?;
            statement
                .query_map([NODES], |row| row.get::<_, String>(0))?
                .map(|payload| {
                    serde_json::from_str::<NodeRecord>(&payload?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        crate::sqlite::validate_node_tree(nodes.clone(), node)
            .map_err(|error| StorageError::Domain(error.to_string()))?;
        let is_new_remote = node.node_id != "desktop-local"
            && !nodes
                .iter()
                .any(|existing| existing.node_id == node.node_id);
        if is_new_remote && remote_node_count(&nodes) >= MAX_REMOTE_NODES {
            return Err(StorageError::Invariant(format!(
                "remote Node capacity is limited to {MAX_REMOTE_NODES}"
            )));
        }
        let active_code_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_node_enrollment_codes WHERE node_id = ?1 AND redeemed_at_ms IS NULL AND expires_at_ms > ?2)",
            params![code.node_id, code.created_at_ms],
            |row| row.get(0),
        )?;
        if active_code_exists {
            return Err(StorageError::Invariant(format!(
                "Node {} already has an active enrollment code",
                code.node_id
            )));
        }
        transaction.execute(
            "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, '', ?3) ON CONFLICT(kind, record_key) DO UPDATE SET payload = excluded.payload, updated_at = unixepoch()",
            params![NODES, node.node_id, serde_json::to_string(node)?],
        )?;
        transaction.execute(
            "INSERT INTO orchestrator_node_enrollment_codes(code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![code.code_id, code.secret_sha256, code.node_id, code.created_at_ms, code.expires_at_ms, code.redeemed_at_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn node_enrollment_code_by_digest(
        &self,
        digest: &str,
    ) -> Result<Option<NodeEnrollmentCode>, StorageError> {
        self.connection()?
            .query_row(
                "SELECT code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms FROM orchestrator_node_enrollment_codes WHERE secret_sha256 = ?1",
                [digest],
                enrollment_from_sqlite,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Resolves an already committed same-CSR replay before the CA signer is
    /// invoked. A pending result is only advisory; first redemption still uses
    /// the transactional compare-and-set below to settle concurrent callers.
    pub fn lookup_node_enrollment(
        &self,
        digest: &str,
        csr_sha256: &str,
    ) -> Result<EnrollmentLookup, StorageError> {
        validate_csr_sha256(csr_sha256).map_err(StorageError::Invariant)?;
        let connection = self.connection()?;
        let ledger = connection
            .query_row(
                "SELECT code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms, redeemed_csr_sha256, issued_certificate_serial FROM orchestrator_node_enrollment_codes WHERE secret_sha256 = ?1",
                [digest],
                |row| {
                    Ok((
                        enrollment_from_sqlite(row)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((code, redeemed_csr_sha256, issued_certificate_serial)) = ledger else {
            return Ok(EnrollmentLookup::NotFound);
        };
        match (
            code.redeemed_at_ms,
            redeemed_csr_sha256,
            issued_certificate_serial,
        ) {
            (None, None, None) => Ok(EnrollmentLookup::Pending(code)),
            (Some(_), Some(redeemed_csr), Some(serial)) if redeemed_csr == csr_sha256 => {
                let issued = connection
                    .query_row(
                        "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = ?1",
                        [&serial],
                        certificate_from_sqlite,
                    )
                    .optional()?
                    .ok_or_else(|| {
                        StorageError::Invariant(format!(
                            "enrollment replay certificate {serial} is missing"
                        ))
                    })?;
                if issued.node_id != code.node_id {
                    return Err(StorageError::Invariant(
                        "enrollment replay certificate belongs to a different Node".into(),
                    ));
                }
                Ok(EnrollmentLookup::Replayed(Box::new(issued)))
            }
            (Some(_), Some(_), Some(_)) | (Some(_), None, None) => {
                Ok(EnrollmentLookup::AlreadyRedeemed)
            }
            _ => Err(StorageError::Invariant(
                "enrollment replay ledger is only partially populated".into(),
            )),
        }
    }

    pub fn redeem_node_enrollment_code(
        &self,
        digest: &str,
        csr_sha256: &str,
        now_ms: i64,
        certificate: NewNodeCertificate,
    ) -> Result<EnrollmentRedemption, StorageError> {
        validate_certificate(&certificate).map_err(StorageError::Invariant)?;
        validate_csr_sha256(csr_sha256).map_err(StorageError::Invariant)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        enforce_sqlite_remote_capacity(&transaction)?;
        let ledger = transaction
            .query_row(
                "SELECT code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms, redeemed_csr_sha256, issued_certificate_serial FROM orchestrator_node_enrollment_codes WHERE secret_sha256 = ?1",
                [digest],
                |row| {
                    Ok((
                        enrollment_from_sqlite(row)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((code, redeemed_csr_sha256, issued_certificate_serial)) = ledger else {
            return Ok(EnrollmentRedemption::NotFound);
        };
        if code.node_id != certificate.node_id {
            return Ok(EnrollmentRedemption::NodeMismatch);
        }
        if code.redeemed_at_ms.is_some() {
            return match (redeemed_csr_sha256, issued_certificate_serial) {
                (Some(redeemed_csr), Some(serial)) if redeemed_csr == csr_sha256 => {
                    let issued = transaction
                        .query_row(
                            "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = ?1",
                            [&serial],
                            certificate_from_sqlite,
                        )
                        .optional()?
                        .ok_or_else(|| {
                            StorageError::Invariant(format!(
                                "enrollment replay certificate {serial} is missing"
                            ))
                        })?;
                    if issued.node_id != code.node_id {
                        return Err(StorageError::Invariant(
                            "enrollment replay certificate belongs to a different Node".into(),
                        ));
                    }
                    Ok(classify_enrollment_replay(Box::new(issued), now_ms))
                }
                (Some(_), Some(_)) | (None, None) => Ok(EnrollmentRedemption::AlreadyRedeemed),
                _ => Err(StorageError::Invariant(
                    "enrollment replay ledger is only partially populated".into(),
                )),
            };
        }
        if now_ms >= code.expires_at_ms {
            return Ok(EnrollmentRedemption::Expired);
        }
        insert_sqlite_certificate(&transaction, &certificate)?;
        let changed = transaction.execute(
            "UPDATE orchestrator_node_enrollment_codes SET redeemed_at_ms = ?2, redeemed_csr_sha256 = ?3, issued_certificate_serial = ?4 WHERE secret_sha256 = ?1 AND redeemed_at_ms IS NULL AND expires_at_ms > ?2",
            params![digest, now_ms, csr_sha256, certificate.serial_hex],
        )?;
        if changed != 1 {
            return Ok(EnrollmentRedemption::AlreadyRedeemed);
        }
        mark_sqlite_node_ready(&transaction, &certificate.node_id, now_ms)?;
        transaction.commit()?;
        Ok(EnrollmentRedemption::Redeemed(Box::new(certificate.into())))
    }

    pub fn node_certificate(
        &self,
        serial_hex: &str,
    ) -> Result<Option<NodeCertificateRecord>, StorageError> {
        self.connection()?
            .query_row(
                "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = ?1",
                [serial_hex],
                certificate_from_sqlite,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn rotate_node_certificate(
        &self,
        current_serial: &str,
        node_id: &str,
        now_ms: i64,
        replacement: NewNodeCertificate,
    ) -> Result<CertificateRotation, StorageError> {
        validate_certificate(&replacement).map_err(StorageError::Invariant)?;
        if replacement.node_id != node_id {
            return Ok(CertificateRotation::NodeMismatch);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = ?1",
                [current_serial],
                certificate_from_sqlite,
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(CertificateRotation::NotFound);
        };
        if current.node_id != node_id {
            return Ok(CertificateRotation::NodeMismatch);
        }
        if current.revoked_at_ms.is_some() {
            return Ok(CertificateRotation::Revoked);
        }
        if now_ms >= current.not_after_ms {
            return Ok(CertificateRotation::Expired);
        }
        let renew_at_ms = current.not_after_ms - CERTIFICATE_RENEWAL_WINDOW_MS;
        if now_ms < renew_at_ms {
            return Ok(CertificateRotation::NotDue { renew_at_ms });
        }
        insert_sqlite_certificate(&transaction, &replacement)?;
        // Keep the authenticated certificate alive until the Agent has
        // durably stored the replacement and proves possession through the
        // activation endpoint. A lost renewal response can therefore be
        // retried instead of permanently locking the Node out.
        transaction.execute(
            "UPDATE orchestrator_node_certificates SET revoked_at_ms = ?3, revoke_reason = 'renewal_response_superseded', replaced_by_serial = ?4 WHERE node_id = ?1 AND serial_hex <> ?2 AND serial_hex <> ?4 AND revoked_at_ms IS NULL",
            params![node_id, current_serial, now_ms, replacement.serial_hex],
        )?;
        let changed = transaction.execute(
            "UPDATE orchestrator_node_certificates SET replaced_by_serial = ?2 WHERE serial_hex = ?1 AND revoked_at_ms IS NULL",
            params![current_serial, replacement.serial_hex],
        )?;
        if changed != 1 {
            return Ok(CertificateRotation::Revoked);
        }
        transaction.commit()?;
        Ok(CertificateRotation::Rotated(replacement.into()))
    }

    pub fn activate_node_certificate(
        &self,
        node_id: &str,
        current_serial: &str,
        now_ms: i64,
    ) -> Result<CertificateActivation, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = ?1",
                [current_serial],
                certificate_from_sqlite,
            )
            .optional()?;
        let Some(current) = current else {
            return Ok(CertificateActivation::NotFound);
        };
        if current.node_id != node_id {
            return Ok(CertificateActivation::NodeMismatch);
        }
        if current.revoked_at_ms.is_some() {
            return Ok(CertificateActivation::Revoked);
        }
        if now_ms >= current.not_after_ms {
            return Ok(CertificateActivation::Expired);
        }
        if current.replaced_by_serial.is_some() {
            return Ok(CertificateActivation::Superseded);
        }
        let revoked = transaction.execute(
            "UPDATE orchestrator_node_certificates SET revoked_at_ms = ?3, revoke_reason = 'superseded', replaced_by_serial = ?2 WHERE node_id = ?1 AND serial_hex <> ?2 AND revoked_at_ms IS NULL",
            params![node_id, current_serial, now_ms],
        )? as u64;
        transaction.commit()?;
        Ok(CertificateActivation::Activated {
            revoked_certificates: revoked,
        })
    }

    pub fn revoke_node_certificates(
        &self,
        node_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<u64, StorageError> {
        if reason.trim().is_empty() {
            return Err(StorageError::Invariant(
                "revocation reason is required".into(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE orchestrator_node_certificates SET revoked_at_ms = ?2, revoke_reason = ?3 WHERE node_id = ?1 AND revoked_at_ms IS NULL",
            params![node_id, now_ms, reason],
        )? as u64;
        mark_sqlite_node_revoked(&transaction, node_id, now_ms)?;
        transaction.commit()?;
        Ok(changed)
    }
}

fn insert_sqlite_certificate(
    transaction: &rusqlite::Transaction<'_>,
    certificate: &NewNodeCertificate,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO orchestrator_node_certificates(serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![certificate.serial_hex, certificate.node_id, certificate.spiffe_id, certificate.certificate_pem, certificate.fingerprint_sha256, certificate.issued_at_ms, certificate.not_before_ms, certificate.not_after_ms],
    )?;
    Ok(())
}

fn mark_sqlite_node_ready(
    transaction: &rusqlite::Transaction<'_>,
    node_id: &str,
    now_ms: i64,
) -> Result<(), StorageError> {
    let payload = transaction
        .query_row(
            "SELECT payload FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2",
            params![NODES, node_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| StorageError::Invariant(format!("enrollment node {node_id} is missing")))?;
    let mut node: NodeRecord = serde_json::from_str(&payload)?;
    if !node.status.eq_ignore_ascii_case("ENROLLMENT_PENDING") {
        return Err(StorageError::Invariant(format!(
            "node {node_id} is not awaiting enrollment"
        )));
    }
    node.status = "READY".to_string();
    node.updated_at = format!("unix-ms:{now_ms}");
    transaction.execute(
        "UPDATE orchestrator_records SET payload = ?3, updated_at = unixepoch() WHERE kind = ?1 AND record_key = ?2",
        params![NODES, node_id, serde_json::to_string(&node)?],
    )?;
    Ok(())
}

fn mark_sqlite_node_revoked(
    transaction: &rusqlite::Transaction<'_>,
    node_id: &str,
    now_ms: i64,
) -> Result<(), StorageError> {
    let Some(payload) = transaction
        .query_row(
            "SELECT payload FROM orchestrator_records WHERE kind = ?1 AND record_key = ?2",
            params![NODES, node_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let mut node: NodeRecord = serde_json::from_str(&payload)?;
    node.status = "AUTH_REVOKED".to_string();
    node.updated_at = format!("unix-ms:{now_ms}");
    transaction.execute(
        "UPDATE orchestrator_records SET payload = ?3, updated_at = unixepoch() WHERE kind = ?1 AND record_key = ?2",
        params![NODES, node_id, serde_json::to_string(&node)?],
    )?;
    Ok(())
}

fn enrollment_from_sqlite(row: &rusqlite::Row<'_>) -> Result<NodeEnrollmentCode, rusqlite::Error> {
    Ok(NodeEnrollmentCode {
        code_id: row.get(0)?,
        secret_sha256: row.get(1)?,
        node_id: row.get(2)?,
        created_at_ms: row.get(3)?,
        expires_at_ms: row.get(4)?,
        redeemed_at_ms: row.get(5)?,
    })
}

fn certificate_from_sqlite(
    row: &rusqlite::Row<'_>,
) -> Result<NodeCertificateRecord, rusqlite::Error> {
    Ok(NodeCertificateRecord {
        serial_hex: row.get(0)?,
        node_id: row.get(1)?,
        spiffe_id: row.get(2)?,
        certificate_pem: row.get(3)?,
        fingerprint_sha256: row.get(4)?,
        issued_at_ms: row.get(5)?,
        not_before_ms: row.get(6)?,
        not_after_ms: row.get(7)?,
        revoked_at_ms: row.get(8)?,
        revoke_reason: row.get(9)?,
        replaced_by_serial: row.get(10)?,
    })
}

impl PostgresOrchestratorStore {
    pub fn register_node_enrollment(
        &self,
        node: &NodeRecord,
        code: &NodeEnrollmentCode,
    ) -> Result<(), PostgresError> {
        validate_code(code).map_err(PostgresError::Invariant)?;
        validate_node_record(node).map_err(|error| PostgresError::Domain(error.to_string()))?;
        if code.node_id != node.node_id || !node.status.eq_ignore_ascii_case("ENROLLMENT_PENDING") {
            return Err(PostgresError::Invariant(
                "enrollment code and pending Node record must identify the same Node".into(),
            ));
        }
        self.pool().with_transaction(|transaction| {
            transaction.query_one(
                "SELECT pg_advisory_xact_lock($1)",
                &[&NODE_ENROLLMENT_CAPACITY_LOCK_KEY],
            )?;
            let nodes = transaction
                .query(
                    "SELECT payload::text FROM orchestrator_records WHERE kind = $1 ORDER BY record_key FOR UPDATE",
                    &[&NODES],
                )?
                .into_iter()
                .map(|row| serde_json::from_str::<NodeRecord>(&row.get::<_, String>(0)))
                .collect::<Result<Vec<_>, _>>()?;
            crate::postgres_store::validate_node_tree(nodes.clone(), node)
                .map_err(|error| PostgresError::Domain(error.to_string()))?;
            let is_new_remote = node.node_id != "desktop-local"
                && !nodes.iter().any(|existing| existing.node_id == node.node_id);
            if is_new_remote && remote_node_count(&nodes) >= MAX_REMOTE_NODES {
                return Err(PostgresError::Invariant(format!(
                    "remote Node capacity is limited to {MAX_REMOTE_NODES}"
                )));
            }
            let active_code_exists = transaction
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM orchestrator_node_enrollment_codes WHERE node_id = $1 AND redeemed_at_ms IS NULL AND expires_at_ms > $2)",
                    &[&code.node_id, &code.created_at_ms],
                )?
                .get::<_, bool>(0);
            if active_code_exists {
                return Err(PostgresError::Invariant(format!(
                    "Node {} already has an active enrollment code",
                    code.node_id
                )));
            }
            let payload = serde_json::to_string(node)?;
            transaction.execute(
                "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES ($1, $2, '', $3::text::jsonb) ON CONFLICT(kind, record_key) DO UPDATE SET payload = excluded.payload, updated_at = clock_timestamp()",
                &[&NODES, &node.node_id, &payload],
            )?;
            transaction.execute(
                "INSERT INTO orchestrator_node_enrollment_codes(code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms) VALUES ($1, $2, $3, $4, $5, $6)",
                &[&code.code_id, &code.secret_sha256, &code.node_id, &code.created_at_ms, &code.expires_at_ms, &code.redeemed_at_ms],
            )?;
            Ok(())
        })
    }

    pub fn node_enrollment_code_by_digest(
        &self,
        digest: &str,
    ) -> Result<Option<NodeEnrollmentCode>, PostgresError> {
        self.pool().with_client(|client| {
            Ok(client
                .query_opt(
                    "SELECT code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms FROM orchestrator_node_enrollment_codes WHERE secret_sha256 = $1",
                    &[&digest],
                )?
                .map(|row| enrollment_from_postgres(&row)))
        })
    }

    pub fn lookup_node_enrollment(
        &self,
        digest: &str,
        csr_sha256: &str,
    ) -> Result<EnrollmentLookup, PostgresError> {
        validate_csr_sha256(csr_sha256).map_err(PostgresError::Invariant)?;
        self.pool().with_client(|client| {
            let ledger = client
                .query_opt(
                    "SELECT code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms, redeemed_csr_sha256, issued_certificate_serial FROM orchestrator_node_enrollment_codes WHERE secret_sha256 = $1",
                    &[&digest],
                )?
                .map(|row| {
                    (
                        enrollment_from_postgres(&row),
                        row.get::<_, Option<String>>(6),
                        row.get::<_, Option<String>>(7),
                    )
                });
            let Some((code, redeemed_csr_sha256, issued_certificate_serial)) = ledger else {
                return Ok(EnrollmentLookup::NotFound);
            };
            match (
                code.redeemed_at_ms,
                redeemed_csr_sha256,
                issued_certificate_serial,
            ) {
                (None, None, None) => Ok(EnrollmentLookup::Pending(code)),
                (Some(_), Some(redeemed_csr), Some(serial)) if redeemed_csr == csr_sha256 => {
                    let issued = client
                        .query_opt(
                            "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = $1",
                            &[&serial],
                        )?
                        .map(|row| certificate_from_postgres(&row))
                        .ok_or_else(|| {
                            PostgresError::Invariant(format!(
                                "enrollment replay certificate {serial} is missing"
                            ))
                        })?;
                    if issued.node_id != code.node_id {
                        return Err(PostgresError::Invariant(
                            "enrollment replay certificate belongs to a different Node".into(),
                        ));
                    }
                    Ok(EnrollmentLookup::Replayed(Box::new(issued)))
                }
                (Some(_), Some(_), Some(_)) | (Some(_), None, None) => {
                    Ok(EnrollmentLookup::AlreadyRedeemed)
                }
                _ => Err(PostgresError::Invariant(
                    "enrollment replay ledger is only partially populated".into(),
                )),
            }
        })
    }

    pub fn redeem_node_enrollment_code(
        &self,
        digest: &str,
        csr_sha256: &str,
        now_ms: i64,
        certificate: NewNodeCertificate,
    ) -> Result<EnrollmentRedemption, PostgresError> {
        validate_certificate(&certificate).map_err(PostgresError::Invariant)?;
        validate_csr_sha256(csr_sha256).map_err(PostgresError::Invariant)?;
        self.pool().with_transaction(|transaction| {
            transaction.query_one(
                "SELECT pg_advisory_xact_lock($1)",
                &[&NODE_ENROLLMENT_CAPACITY_LOCK_KEY],
            )?;
            enforce_postgres_remote_capacity(transaction)?;
            let ledger = transaction
                .query_opt(
                    "SELECT code_id, secret_sha256, node_id, created_at_ms, expires_at_ms, redeemed_at_ms, redeemed_csr_sha256, issued_certificate_serial FROM orchestrator_node_enrollment_codes WHERE secret_sha256 = $1 FOR UPDATE",
                    &[&digest],
                )?
                .map(|row| {
                    (
                        enrollment_from_postgres(&row),
                        row.get::<_, Option<String>>(6),
                        row.get::<_, Option<String>>(7),
                    )
                });
            let Some((code, redeemed_csr_sha256, issued_certificate_serial)) = ledger else {
                return Ok(EnrollmentRedemption::NotFound);
            };
            if code.node_id != certificate.node_id {
                return Ok(EnrollmentRedemption::NodeMismatch);
            }
            if code.redeemed_at_ms.is_some() {
                return match (redeemed_csr_sha256, issued_certificate_serial) {
                    (Some(redeemed_csr), Some(serial)) if redeemed_csr == csr_sha256 => {
                        let issued = transaction
                            .query_opt(
                                "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = $1",
                                &[&serial],
                            )?
                            .map(|row| certificate_from_postgres(&row))
                            .ok_or_else(|| {
                                PostgresError::Invariant(format!(
                                    "enrollment replay certificate {serial} is missing"
                                ))
                            })?;
                        if issued.node_id != code.node_id {
                            return Err(PostgresError::Invariant(
                                "enrollment replay certificate belongs to a different Node".into(),
                            ));
                        }
                        Ok(classify_enrollment_replay(Box::new(issued), now_ms))
                    }
                    (Some(_), Some(_)) | (None, None) => {
                        Ok(EnrollmentRedemption::AlreadyRedeemed)
                    }
                    _ => Err(PostgresError::Invariant(
                        "enrollment replay ledger is only partially populated".into(),
                    )),
                };
            }
            if now_ms >= code.expires_at_ms {
                return Ok(EnrollmentRedemption::Expired);
            }
            insert_postgres_certificate(transaction, &certificate)?;
            let changed = transaction.execute(
                "UPDATE orchestrator_node_enrollment_codes SET redeemed_at_ms = $2, redeemed_csr_sha256 = $3, issued_certificate_serial = $4 WHERE secret_sha256 = $1",
                &[&digest, &now_ms, &csr_sha256, &certificate.serial_hex],
            )?;
            if changed != 1 {
                return Err(PostgresError::Invariant(
                    "enrollment redemption lost its locked ledger row".into(),
                ));
            }
            mark_postgres_node_ready(transaction, &certificate.node_id, now_ms)?;
            Ok(EnrollmentRedemption::Redeemed(Box::new(certificate.into())))
        })
    }

    pub fn node_certificate(
        &self,
        serial_hex: &str,
    ) -> Result<Option<NodeCertificateRecord>, PostgresError> {
        self.pool().with_client(|client| {
            Ok(client
                .query_opt(
                    "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = $1",
                    &[&serial_hex],
                )?
                .map(|row| certificate_from_postgres(&row)))
        })
    }

    pub fn rotate_node_certificate(
        &self,
        current_serial: &str,
        node_id: &str,
        now_ms: i64,
        replacement: NewNodeCertificate,
    ) -> Result<CertificateRotation, PostgresError> {
        validate_certificate(&replacement).map_err(PostgresError::Invariant)?;
        if replacement.node_id != node_id {
            return Ok(CertificateRotation::NodeMismatch);
        }
        self.pool().with_transaction(|transaction| {
            let current = transaction
                .query_opt(
                    "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = $1 FOR UPDATE",
                    &[&current_serial],
                )?
                .map(|row| certificate_from_postgres(&row));
            let Some(current) = current else {
                return Ok(CertificateRotation::NotFound);
            };
            if current.node_id != node_id {
                return Ok(CertificateRotation::NodeMismatch);
            }
            if current.revoked_at_ms.is_some() {
                return Ok(CertificateRotation::Revoked);
            }
            if now_ms >= current.not_after_ms {
                return Ok(CertificateRotation::Expired);
            }
            let renew_at_ms = current.not_after_ms - CERTIFICATE_RENEWAL_WINDOW_MS;
            if now_ms < renew_at_ms {
                return Ok(CertificateRotation::NotDue { renew_at_ms });
            }
            insert_postgres_certificate(transaction, &replacement)?;
            transaction.execute(
                "UPDATE orchestrator_node_certificates SET revoked_at_ms = $3, revoke_reason = 'renewal_response_superseded', replaced_by_serial = $4 WHERE node_id = $1 AND serial_hex <> $2 AND serial_hex <> $4 AND revoked_at_ms IS NULL",
                &[&node_id, &current_serial, &now_ms, &replacement.serial_hex],
            )?;
            transaction.execute(
                "UPDATE orchestrator_node_certificates SET replaced_by_serial = $2 WHERE serial_hex = $1 AND revoked_at_ms IS NULL",
                &[&current_serial, &replacement.serial_hex],
            )?;
            Ok(CertificateRotation::Rotated(replacement.into()))
        })
    }

    pub fn activate_node_certificate(
        &self,
        node_id: &str,
        current_serial: &str,
        now_ms: i64,
    ) -> Result<CertificateActivation, PostgresError> {
        self.pool().with_transaction(|transaction| {
            let current = transaction
                .query_opt(
                    "SELECT serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms, revoked_at_ms, revoke_reason, replaced_by_serial FROM orchestrator_node_certificates WHERE serial_hex = $1 FOR UPDATE",
                    &[&current_serial],
                )?
                .map(|row| certificate_from_postgres(&row));
            let Some(current) = current else {
                return Ok(CertificateActivation::NotFound);
            };
            if current.node_id != node_id {
                return Ok(CertificateActivation::NodeMismatch);
            }
            if current.revoked_at_ms.is_some() {
                return Ok(CertificateActivation::Revoked);
            }
            if now_ms >= current.not_after_ms {
                return Ok(CertificateActivation::Expired);
            }
            if current.replaced_by_serial.is_some() {
                return Ok(CertificateActivation::Superseded);
            }
            let revoked = transaction.execute(
                "UPDATE orchestrator_node_certificates SET revoked_at_ms = $3, revoke_reason = 'superseded', replaced_by_serial = $2 WHERE node_id = $1 AND serial_hex <> $2 AND revoked_at_ms IS NULL",
                &[&node_id, &current_serial, &now_ms],
            )?;
            Ok(CertificateActivation::Activated {
                revoked_certificates: revoked,
            })
        })
    }

    pub fn revoke_node_certificates(
        &self,
        node_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<u64, PostgresError> {
        if reason.trim().is_empty() {
            return Err(PostgresError::Invariant(
                "revocation reason is required".into(),
            ));
        }
        self.pool().with_transaction(|transaction| {
            let changed = transaction.execute(
                "UPDATE orchestrator_node_certificates SET revoked_at_ms = $2, revoke_reason = $3 WHERE node_id = $1 AND revoked_at_ms IS NULL",
                &[&node_id, &now_ms, &reason],
            )?;
            mark_postgres_node_revoked(transaction, node_id, now_ms)?;
            Ok(changed)
        })
    }
}

fn insert_postgres_certificate(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    certificate: &NewNodeCertificate,
) -> Result<(), PostgresError> {
    transaction.execute(
        "INSERT INTO orchestrator_node_certificates(serial_hex, node_id, spiffe_id, certificate_pem, fingerprint_sha256, issued_at_ms, not_before_ms, not_after_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[&certificate.serial_hex, &certificate.node_id, &certificate.spiffe_id, &certificate.certificate_pem, &certificate.fingerprint_sha256, &certificate.issued_at_ms, &certificate.not_before_ms, &certificate.not_after_ms],
    )?;
    Ok(())
}

fn mark_postgres_node_ready(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    node_id: &str,
    now_ms: i64,
) -> Result<(), PostgresError> {
    let row = transaction
        .query_opt(
            "SELECT payload::text FROM orchestrator_records WHERE kind = $1 AND record_key = $2 FOR UPDATE",
            &[&NODES, &node_id],
        )?
        .ok_or_else(|| PostgresError::Invariant(format!("enrollment node {node_id} is missing")))?;
    let mut node: NodeRecord = serde_json::from_str(&row.get::<_, String>(0))?;
    if !node.status.eq_ignore_ascii_case("ENROLLMENT_PENDING") {
        return Err(PostgresError::Invariant(format!(
            "node {node_id} is not awaiting enrollment"
        )));
    }
    node.status = "READY".to_string();
    node.updated_at = format!("unix-ms:{now_ms}");
    let payload = serde_json::to_string(&node)?;
    transaction.execute(
        "UPDATE orchestrator_records SET payload = $3::text::jsonb, updated_at = clock_timestamp() WHERE kind = $1 AND record_key = $2",
        &[&NODES, &node_id, &payload],
    )?;
    Ok(())
}

fn mark_postgres_node_revoked(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    node_id: &str,
    now_ms: i64,
) -> Result<(), PostgresError> {
    let Some(row) = transaction.query_opt(
        "SELECT payload::text FROM orchestrator_records WHERE kind = $1 AND record_key = $2 FOR UPDATE",
        &[&NODES, &node_id],
    )? else {
        return Ok(());
    };
    let mut node: NodeRecord = serde_json::from_str(&row.get::<_, String>(0))?;
    node.status = "AUTH_REVOKED".to_string();
    node.updated_at = format!("unix-ms:{now_ms}");
    let payload = serde_json::to_string(&node)?;
    transaction.execute(
        "UPDATE orchestrator_records SET payload = $3::text::jsonb, updated_at = clock_timestamp() WHERE kind = $1 AND record_key = $2",
        &[&NODES, &node_id, &payload],
    )?;
    Ok(())
}

fn enrollment_from_postgres(row: &r2d2_postgres::postgres::Row) -> NodeEnrollmentCode {
    NodeEnrollmentCode {
        code_id: row.get(0),
        secret_sha256: row.get(1),
        node_id: row.get(2),
        created_at_ms: row.get(3),
        expires_at_ms: row.get(4),
        redeemed_at_ms: row.get(5),
    }
}

fn certificate_from_postgres(row: &r2d2_postgres::postgres::Row) -> NodeCertificateRecord {
    NodeCertificateRecord {
        serial_hex: row.get(0),
        node_id: row.get(1),
        spiffe_id: row.get(2),
        certificate_pem: row.get(3),
        fingerprint_sha256: row.get(4),
        issued_at_ms: row.get(5),
        not_before_ms: row.get(6),
        not_after_ms: row.get(7),
        revoked_at_ms: row.get(8),
        revoke_reason: row.get(9),
        replaced_by_serial: row.get(10),
    }
}

fn remote_node_count(nodes: &[NodeRecord]) -> usize {
    nodes
        .iter()
        .filter(|node| {
            node.node_id != "desktop-local" && !node.status.eq_ignore_ascii_case("REMOVED")
        })
        .count()
}

fn enforce_sqlite_remote_capacity(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), StorageError> {
    let count: usize = transaction.query_row(
        "SELECT COUNT(*) FROM orchestrator_records WHERE kind = ?1 AND json_extract(payload, '$.node_id') <> 'desktop-local' AND UPPER(json_extract(payload, '$.status')) <> 'REMOVED'",
        [NODES],
        |row| row.get(0),
    )?;
    if count > MAX_REMOTE_NODES {
        return Err(StorageError::Invariant(format!(
            "remote Node capacity exceeds {MAX_REMOTE_NODES}"
        )));
    }
    Ok(())
}

fn enforce_postgres_remote_capacity(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
) -> Result<(), PostgresError> {
    let count: i64 = transaction
        .query_one(
            "SELECT COUNT(*) FROM orchestrator_records WHERE kind = $1 AND payload->>'node_id' <> 'desktop-local' AND UPPER(payload->>'status') <> 'REMOVED'",
            &[&NODES],
        )?
        .get(0);
    if count > MAX_REMOTE_NODES as i64 {
        return Err(PostgresError::Invariant(format!(
            "remote Node capacity exceeds {MAX_REMOTE_NODES}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::OrchestratorStore;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::tempdir;

    fn certificate(serial: &str, node_id: &str, issued_at_ms: i64) -> NewNodeCertificate {
        NewNodeCertificate {
            serial_hex: serial.to_string(),
            node_id: node_id.to_string(),
            spiffe_id: format!("spiffe://ojos.local/node/{node_id}"),
            certificate_pem: format!("certificate-{serial}"),
            fingerprint_sha256: format!("sha256:fingerprint-{serial}"),
            issued_at_ms,
            not_before_ms: issued_at_ms,
            not_after_ms: issued_at_ms + CERTIFICATE_LIFETIME_MS,
        }
    }

    fn csr_sha256(marker: char) -> String {
        format!("sha256:{}", marker.to_string().repeat(64))
    }

    #[test]
    fn sqlite_enrollment_code_is_consumed_exactly_once_under_concurrency() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("identity.db")).unwrap();
        seed_node(&store);
        store
            .register_node_enrollment(
                &pending_node(),
                &NodeEnrollmentCode {
                    code_id: "code-1".into(),
                    secret_sha256: "sha256:secret".into(),
                    node_id: "node-1".into(),
                    created_at_ms: 1,
                    expires_at_ms: 10_000,
                    redeemed_at_ms: None,
                },
            )
            .unwrap();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(8));
        let same_csr_sha256 = csr_sha256('a');
        let handles = (0..8)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                let csr_sha256 = same_csr_sha256.clone();
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .redeem_node_enrollment_code(
                            "sha256:secret",
                            &csr_sha256,
                            2,
                            certificate(&format!("{index:02x}"), "node-1", 2),
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EnrollmentRedemption::Redeemed(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, EnrollmentRedemption::Replayed(_)))
                .count(),
            7
        );
        let serials = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                EnrollmentRedemption::Redeemed(certificate)
                | EnrollmentRedemption::Replayed(certificate) => {
                    Some(certificate.serial_hex.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            serials.len(),
            1,
            "all retries return the committed certificate"
        );
        assert!(matches!(
            store
                .redeem_node_enrollment_code(
                    "sha256:secret",
                    &csr_sha256('b'),
                    3,
                    certificate("different-csr", "node-1", 3),
                )
                .unwrap(),
            EnrollmentRedemption::AlreadyRedeemed
        ));
        store
            .revoke_node_certificates("node-1", 4, "operator revoked")
            .unwrap();
        assert!(matches!(
            store
                .redeem_node_enrollment_code(
                    "sha256:secret",
                    &same_csr_sha256,
                    5,
                    certificate("race-loser", "node-1", 5),
                )
                .unwrap(),
            EnrollmentRedemption::ReplayCertificateRevoked
        ));
    }

    #[test]
    fn sqlite_rotation_is_bounded_and_activation_revokes_the_old_serial() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("identity.db")).unwrap();
        seed_node(&store);
        store
            .register_node_enrollment(
                &pending_node(),
                &NodeEnrollmentCode {
                    code_id: "code-1".into(),
                    secret_sha256: "sha256:secret".into(),
                    node_id: "node-1".into(),
                    created_at_ms: 1,
                    expires_at_ms: 10_000,
                    redeemed_at_ms: None,
                },
            )
            .unwrap();
        store
            .redeem_node_enrollment_code(
                "sha256:secret",
                &csr_sha256('a'),
                2,
                certificate("01", "node-1", 2),
            )
            .unwrap();
        let early = store
            .rotate_node_certificate("01", "node-1", 3, certificate("02", "node-1", 3))
            .unwrap();
        assert!(matches!(early, CertificateRotation::NotDue { .. }));
        let renew_at = 2 + CERTIFICATE_LIFETIME_MS - CERTIFICATE_RENEWAL_WINDOW_MS;
        let rotated = store
            .rotate_node_certificate(
                "01",
                "node-1",
                renew_at,
                certificate("03", "node-1", renew_at),
            )
            .unwrap();
        assert!(matches!(rotated, CertificateRotation::Rotated(_)));
        let old = store.node_certificate("01").unwrap().unwrap();
        assert_eq!(old.revoked_at_ms, None);
        assert_eq!(old.replaced_by_serial.as_deref(), Some("03"));

        let activated = store
            .activate_node_certificate("node-1", "03", renew_at + 1)
            .unwrap();
        assert_eq!(
            activated,
            CertificateActivation::Activated {
                revoked_certificates: 1
            }
        );
        assert_eq!(
            store
                .node_certificate("01")
                .unwrap()
                .unwrap()
                .revoke_reason
                .as_deref(),
            Some("superseded")
        );
    }

    #[test]
    fn sqlite_lost_renewal_response_can_be_retried_with_the_old_certificate() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("identity.db")).unwrap();
        seed_node(&store);
        store
            .register_node_enrollment(
                &pending_node(),
                &NodeEnrollmentCode {
                    code_id: "code-retry".into(),
                    secret_sha256: "sha256:retry".into(),
                    node_id: "node-1".into(),
                    created_at_ms: 1,
                    expires_at_ms: 10_000,
                    redeemed_at_ms: None,
                },
            )
            .unwrap();
        store
            .redeem_node_enrollment_code(
                "sha256:retry",
                &csr_sha256('a'),
                2,
                certificate("11", "node-1", 2),
            )
            .unwrap();
        let renew_at = 2 + CERTIFICATE_LIFETIME_MS - CERTIFICATE_RENEWAL_WINDOW_MS;
        store
            .rotate_node_certificate(
                "11",
                "node-1",
                renew_at,
                certificate("12", "node-1", renew_at),
            )
            .unwrap();

        // Simulate a response lost before the replacement key/certificate was
        // committed by the Agent. The still-active old identity retries with a
        // fresh CSR; the orphan response is invalidated deterministically.
        store
            .rotate_node_certificate(
                "11",
                "node-1",
                renew_at + 1,
                certificate("13", "node-1", renew_at + 1),
            )
            .unwrap();
        assert_eq!(
            store
                .node_certificate("12")
                .unwrap()
                .unwrap()
                .revoke_reason
                .as_deref(),
            Some("renewal_response_superseded")
        );
        assert_eq!(
            store.node_certificate("11").unwrap().unwrap().revoked_at_ms,
            None
        );
        assert!(matches!(
            store
                .activate_node_certificate("node-1", "13", renew_at + 2)
                .unwrap(),
            CertificateActivation::Activated {
                revoked_certificates: 1
            }
        ));
    }

    #[test]
    fn sqlite_expired_enrollment_code_cannot_issue_a_certificate() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("identity.db")).unwrap();
        seed_node(&store);
        store
            .register_node_enrollment(
                &pending_node(),
                &NodeEnrollmentCode {
                    code_id: "code-expired".into(),
                    secret_sha256: "sha256:expired".into(),
                    node_id: "node-1".into(),
                    created_at_ms: 1,
                    expires_at_ms: 10,
                    redeemed_at_ms: None,
                },
            )
            .unwrap();

        let outcome = store
            .redeem_node_enrollment_code(
                "sha256:expired",
                &csr_sha256('a'),
                10,
                certificate("expired", "node-1", 10),
            )
            .unwrap();

        assert!(matches!(outcome, EnrollmentRedemption::Expired));
        assert!(store.node_certificate("expired").unwrap().is_none());
        assert_eq!(
            store.get_node("node-1").unwrap().unwrap().status,
            "ENROLLMENT_PENDING"
        );
    }

    #[test]
    fn sqlite_remote_node_capacity_is_exactly_one_hundred_and_excludes_desktop() {
        let directory = tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(directory.path().join("identity.db")).unwrap();
        use orchestrator_legacy::OrchestratorStore;
        let mut core_store = store.clone();
        core_store
            .upsert_node(NodeRecord {
                node_id: "desktop-local".into(),
                host_ip: "127.0.0.1".into(),
                parent_node_id: String::new(),
                role: "standalone".into(),
                labels: serde_json::json!({}),
                status: "READY".into(),
                created_at: "unix-ms:0".into(),
                updated_at: "unix-ms:0".into(),
            })
            .unwrap();
        for index in 0..MAX_REMOTE_NODES {
            let node_id = format!("node-{index:03}");
            let node = NodeRecord {
                node_id: node_id.clone(),
                host_ip: format!("10.0.{}.{}", index / 250, index % 250 + 1),
                parent_node_id: String::new(),
                role: "standalone".into(),
                labels: serde_json::json!({}),
                status: "ENROLLMENT_PENDING".into(),
                created_at: "unix-ms:1".into(),
                updated_at: "unix-ms:1".into(),
            };
            store
                .register_node_enrollment(
                    &node,
                    &NodeEnrollmentCode {
                        code_id: format!("code-{index:03}"),
                        secret_sha256: format!("sha256:{index:064x}"),
                        node_id,
                        created_at_ms: 1,
                        expires_at_ms: 10_000,
                        redeemed_at_ms: None,
                    },
                )
                .unwrap();
        }
        let overflow = NodeRecord {
            node_id: "node-overflow".into(),
            host_ip: "10.1.0.1".into(),
            parent_node_id: String::new(),
            role: "standalone".into(),
            labels: serde_json::json!({}),
            status: "ENROLLMENT_PENDING".into(),
            created_at: "unix-ms:1".into(),
            updated_at: "unix-ms:1".into(),
        };
        let error = store
            .register_node_enrollment(
                &overflow,
                &NodeEnrollmentCode {
                    code_id: "overflow".into(),
                    secret_sha256: format!("sha256:{}", "f".repeat(64)),
                    node_id: overflow.node_id.clone(),
                    created_at_ms: 1,
                    expires_at_ms: 10_000,
                    redeemed_at_ms: None,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("limited to 100"));
    }

    fn seed_node(store: &SqliteOrchestratorStore) {
        use orchestrator_legacy::OrchestratorStore;
        let mut store = store.clone();
        store
            .upsert_node(NodeRecord {
                node_id: "node-1".into(),
                host_ip: "127.0.0.2".into(),
                parent_node_id: String::new(),
                role: "standalone".into(),
                labels: serde_json::json!({}),
                status: "ENROLLMENT_PENDING".into(),
                created_at: "unix-ms:1".into(),
                updated_at: "unix-ms:1".into(),
            })
            .unwrap();
    }

    fn pending_node() -> NodeRecord {
        NodeRecord {
            node_id: "node-1".into(),
            host_ip: "127.0.0.2".into(),
            parent_node_id: String::new(),
            role: "standalone".into(),
            labels: serde_json::json!({}),
            status: "ENROLLMENT_PENDING".into(),
            created_at: "unix-ms:1".into(),
            updated_at: "unix-ms:1".into(),
        }
    }
}
