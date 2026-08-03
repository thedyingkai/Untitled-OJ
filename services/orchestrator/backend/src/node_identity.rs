use crate::durable::{DurableError, DurableStore};
use anyhow::{Context, Result, anyhow};
use getrandom::fill as random_fill;
use orchestrator_storage::{
    CERTIFICATE_LIFETIME_MS, CertificateRotation, EnrollmentLookup, EnrollmentRedemption,
    NewNodeCertificate, NodeCertificateRecord, classify_enrollment_replay,
};
use rcgen::{
    CertificateSigningRequestParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType, SerialNumber,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use time::{Duration as TimeDuration, OffsetDateTime};
use x509_parser::extensions::GeneralName;
use x509_parser::parse_x509_certificate;

const SPIFFE_PREFIX: &str = "spiffe://ojos.local/node/";
const TLS_CERT_ENV: &str = "ORCHESTRATOR_TLS_CERT";
const TLS_KEY_ENV: &str = "ORCHESTRATOR_TLS_KEY";
const NODE_CA_CERT_ENV: &str = "ORCHESTRATOR_NODE_CA_CERT";
const NODE_CA_KEY_ENV: &str = "ORCHESTRATOR_NODE_CA_KEY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodePeerIdentity {
    pub(crate) node_id: String,
    pub(crate) spiffe_id: String,
    pub(crate) serial_hex: String,
    pub(crate) fingerprint_sha256: String,
}

impl NodePeerIdentity {
    pub(crate) fn from_certificate_der(der: &[u8]) -> Result<Self> {
        let (_, certificate) = parse_x509_certificate(der)
            .map_err(|_| anyhow!("mTLS peer certificate is not valid X.509 DER"))?;
        let san = certificate
            .subject_alternative_name()
            .context("read mTLS peer certificate subjectAltName")?
            .ok_or_else(|| anyhow!("mTLS peer certificate has no subjectAltName"))?;
        let spiffe_ids = san
            .value
            .general_names
            .iter()
            .filter_map(|name| match name {
                GeneralName::URI(uri) if uri.starts_with(SPIFFE_PREFIX) => Some((*uri).to_string()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if spiffe_ids.len() != 1 {
            return Err(anyhow!(
                "mTLS peer certificate must contain exactly one OJOS Node SPIFFE URI SAN"
            ));
        }
        let spiffe_id = spiffe_ids.into_iter().next().expect("length checked");
        let node_id = spiffe_id
            .strip_prefix(SPIFFE_PREFIX)
            .expect("filtered by prefix")
            .to_string();
        validate_node_id(&node_id)?;
        Ok(Self {
            node_id,
            spiffe_id,
            serial_hex: normalize_serial(&certificate.raw_serial_as_string()),
            fingerprint_sha256: format!("sha256:{:x}", Sha256::digest(der)),
        })
    }
}

pub(crate) struct NodeIdentityService {
    issuer: Mutex<Issuer<'static, KeyPair>>,
    ca_certificate_pem: Arc<str>,
    server_config: Arc<ServerConfig>,
}

impl std::fmt::Debug for NodeIdentityService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeIdentityService")
            .field("ca_certificate_loaded", &true)
            .field("tls_server_configured", &true)
            .finish_non_exhaustive()
    }
}

impl NodeIdentityService {
    pub(crate) fn from_env(required: bool) -> Result<Option<Arc<Self>>> {
        let paths = [
            (TLS_CERT_ENV, env_path(TLS_CERT_ENV)),
            (TLS_KEY_ENV, env_path(TLS_KEY_ENV)),
            (NODE_CA_CERT_ENV, env_path(NODE_CA_CERT_ENV)),
            (NODE_CA_KEY_ENV, env_path(NODE_CA_KEY_ENV)),
        ];
        if paths.iter().all(|(_, value)| value.is_none()) {
            return if required {
                Err(anyhow!(
                    "production PostgreSQL mode requires {TLS_CERT_ENV}, {TLS_KEY_ENV}, {NODE_CA_CERT_ENV}, and {NODE_CA_KEY_ENV}"
                ))
            } else {
                Ok(None)
            };
        }
        let missing = paths
            .iter()
            .filter_map(|(name, value)| value.is_none().then_some(*name))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(anyhow!(
                "Node TLS identity configuration is incomplete; missing {}",
                missing.join(", ")
            ));
        }
        let service = Self::from_pem_files(
            paths[0].1.as_ref().expect("validated"),
            paths[1].1.as_ref().expect("validated"),
            paths[2].1.as_ref().expect("validated"),
            paths[3].1.as_ref().expect("validated"),
        )?;
        Ok(Some(Arc::new(service)))
    }

    fn from_pem_files(
        tls_certificate_path: &Path,
        tls_key_path: &Path,
        ca_certificate_path: &Path,
        ca_key_path: &Path,
    ) -> Result<Self> {
        let tls_certificate = fs::read(tls_certificate_path)
            .with_context(|| format!("read TLS certificate {}", tls_certificate_path.display()))?;
        let tls_key = fs::read(tls_key_path)
            .with_context(|| format!("read TLS private key {}", tls_key_path.display()))?;
        let ca_certificate = fs::read_to_string(ca_certificate_path).with_context(|| {
            format!("read Node CA certificate {}", ca_certificate_path.display())
        })?;
        let ca_key = fs::read_to_string(ca_key_path)
            .with_context(|| format!("read Node CA private key {}", ca_key_path.display()))?;
        Self::from_pem(&tls_certificate, &tls_key, &ca_certificate, &ca_key)
    }

    pub(crate) fn from_pem(
        tls_certificate_pem: &[u8],
        tls_key_pem: &[u8],
        ca_certificate_pem: &str,
        ca_key_pem: &str,
    ) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca_key = KeyPair::from_pem(ca_key_pem).context("parse Node CA private key")?;
        let issuer = Issuer::from_ca_cert_pem(ca_certificate_pem, ca_key)
            .context("parse Node CA certificate")?;

        let ca_certificates = pem_certificates(ca_certificate_pem.as_bytes())?;
        if ca_certificates.is_empty() {
            return Err(anyhow!("Node CA PEM contains no certificate"));
        }
        let mut roots = RootCertStore::empty();
        for certificate in ca_certificates {
            roots
                .add(certificate)
                .context("add Node CA certificate to client trust roots")?;
        }
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .allow_unauthenticated()
            .build()
            .context("build optional mTLS client verifier")?;
        let server_certificates = pem_certificates(tls_certificate_pem)?;
        if server_certificates.is_empty() {
            return Err(anyhow!("TLS certificate PEM contains no certificate"));
        }
        let server_key = pem_private_key(tls_key_pem)?;
        let server_config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(server_certificates, server_key)
            .context("build TLS server configuration")?;
        Ok(Self {
            issuer: Mutex::new(issuer),
            ca_certificate_pem: Arc::from(ca_certificate_pem.to_string()),
            server_config: Arc::new(server_config),
        })
    }

    pub(crate) fn server_config(&self) -> Arc<ServerConfig> {
        Arc::clone(&self.server_config)
    }

    pub(crate) fn ca_certificate_pem(&self) -> &str {
        &self.ca_certificate_pem
    }

    pub(crate) fn issue(
        &self,
        node_id: &str,
        csr_pem: &str,
        issued_at_ms: i64,
    ) -> Result<NewNodeCertificate> {
        validate_node_id(node_id)?;
        if csr_pem.len() > 64 * 1024 {
            return Err(anyhow!("CSR exceeds the 64 KiB limit"));
        }
        let mut request = CertificateSigningRequestParams::from_pem(csr_pem)
            .context("parse and verify Node certificate signing request")?;
        let spiffe_id = format!("{SPIFFE_PREFIX}{node_id}");
        let serial_bytes = random_serial()?;
        let serial_hex = hex(&serial_bytes);
        let not_before = OffsetDateTime::from_unix_timestamp(issued_at_ms.div_euclid(1_000))
            .context("certificate issue timestamp is out of range")?;
        let not_after = not_before
            .checked_add(TimeDuration::milliseconds(CERTIFICATE_LIFETIME_MS))
            .ok_or_else(|| anyhow!("certificate expiry timestamp overflow"))?;

        request.params.not_before = not_before;
        request.params.not_after = not_after;
        request.params.serial_number = Some(SerialNumber::from(serial_bytes));
        request.params.subject_alt_names = vec![SanType::URI(
            spiffe_id
                .as_str()
                .try_into()
                .context("SPIFFE URI is not valid IA5 text")?,
        )];
        request.params.distinguished_name = DistinguishedName::new();
        request
            .params
            .distinguished_name
            .push(DnType::CommonName, node_id);
        request.params.is_ca = IsCa::NoCa;
        request.params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        request.params.name_constraints = None;
        request.params.crl_distribution_points.clear();
        request.params.custom_extensions.clear();
        request.params.use_authority_key_identifier_extension = true;

        let issuer = self
            .issuer
            .lock()
            .map_err(|_| anyhow!("Node CA signer lock is unavailable"))?;
        let certificate = request
            .signed_by(&issuer)
            .context("sign Node client certificate")?;
        let certificate_der = certificate.der().as_ref();
        Ok(NewNodeCertificate {
            serial_hex,
            node_id: node_id.to_string(),
            spiffe_id,
            certificate_pem: certificate.pem(),
            fingerprint_sha256: format!("sha256:{:x}", Sha256::digest(certificate_der)),
            issued_at_ms,
            not_before_ms: issued_at_ms,
            not_after_ms: issued_at_ms.saturating_add(CERTIFICATE_LIFETIME_MS),
        })
    }

    pub(crate) fn authenticate(
        &self,
        store: &DurableStore,
        peer: &NodePeerIdentity,
        now_ms: i64,
    ) -> Result<NodeCertificateRecord, NodeAuthenticationError> {
        let record = store
            .node_certificate(&peer.serial_hex)
            .map_err(NodeAuthenticationError::Storage)?
            .ok_or(NodeAuthenticationError::UnknownSerial)?;
        if record.node_id != peer.node_id
            || record.spiffe_id != peer.spiffe_id
            || record.fingerprint_sha256 != peer.fingerprint_sha256
        {
            return Err(NodeAuthenticationError::IdentityMismatch);
        }
        if record.revoked_at_ms.is_some() {
            return Err(NodeAuthenticationError::Revoked);
        }
        if now_ms < record.not_before_ms || now_ms >= record.not_after_ms {
            return Err(NodeAuthenticationError::Expired);
        }
        Ok(record)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum NodeAuthenticationError {
    #[error("mTLS certificate serial is not registered")]
    UnknownSerial,
    #[error("mTLS certificate identity does not match its durable ledger record")]
    IdentityMismatch,
    #[error("mTLS certificate has been revoked")]
    Revoked,
    #[error("mTLS certificate is outside its validity period")]
    Expired,
    #[error("Node identity ledger is unavailable: {0}")]
    Storage(DurableError),
}

pub(crate) fn redeem(
    store: &DurableStore,
    identity: &NodeIdentityService,
    enrollment_code: &str,
    csr_pem: &str,
    now_ms: i64,
) -> Result<EnrollmentRedemption> {
    redeem_with_issuer(
        store,
        enrollment_code,
        csr_pem,
        now_ms,
        |node_id, csr, now| identity.issue(node_id, csr, now),
    )
}

fn redeem_with_issuer<F>(
    store: &DurableStore,
    enrollment_code: &str,
    csr_pem: &str,
    now_ms: i64,
    issue: F,
) -> Result<EnrollmentRedemption>
where
    F: FnOnce(&str, &str, i64) -> Result<NewNodeCertificate>,
{
    let digest = secret_digest(enrollment_code);
    let csr_sha256 = secret_digest(csr_pem);
    let code = match store
        .lookup_node_enrollment(&digest, &csr_sha256)
        .context("read enrollment replay ledger")?
    {
        EnrollmentLookup::Pending(code) => code,
        EnrollmentLookup::Replayed(certificate) => {
            return Ok(classify_enrollment_replay(certificate, now_ms));
        }
        EnrollmentLookup::NotFound => return Ok(EnrollmentRedemption::NotFound),
        EnrollmentLookup::AlreadyRedeemed => {
            return Ok(EnrollmentRedemption::AlreadyRedeemed);
        }
    };
    if now_ms >= code.expires_at_ms {
        return Ok(EnrollmentRedemption::Expired);
    }
    let certificate = issue(&code.node_id, csr_pem, now_ms)?;
    store
        .redeem_node_enrollment_code(&digest, &csr_sha256, now_ms, certificate)
        .context("redeem enrollment code")
}

pub(crate) fn renew(
    store: &DurableStore,
    identity: &NodeIdentityService,
    peer: &NodePeerIdentity,
    csr_pem: &str,
    now_ms: i64,
) -> Result<CertificateRotation> {
    identity
        .authenticate(store, peer, now_ms)
        .context("authenticate certificate being renewed")?;
    let replacement = identity.issue(&peer.node_id, csr_pem, now_ms)?;
    store
        .rotate_node_certificate(&peer.serial_hex, &peer.node_id, now_ms, replacement)
        .context("rotate Node certificate")
}

pub(crate) fn secret_digest(secret: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(secret.as_bytes()))
}

pub(crate) fn random_secret(prefix: &str) -> Result<String> {
    let mut bytes = [0_u8; 32];
    random_fill(&mut bytes)
        .map_err(|_| anyhow!("operating system random source is unavailable"))?;
    Ok(format!("{prefix}{}", hex(&bytes)))
}

fn random_serial() -> Result<Vec<u8>> {
    let mut bytes = [0_u8; 20];
    random_fill(&mut bytes)
        .map_err(|_| anyhow!("operating system random source is unavailable"))?;
    bytes[0] &= 0x7f;
    if bytes[0] == 0 {
        bytes[0] = 1;
    }
    Ok(bytes.to_vec())
}

fn validate_node_id(node_id: &str) -> Result<()> {
    if node_id.is_empty()
        || node_id.len() > 128
        || !node_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(anyhow!(
            "node_id must contain 1-128 ASCII letters, digits, '.', '_', or '-'"
        ));
    }
    Ok(())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn pem_certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    rustls_pemfile::certs(&mut BufReader::new(Cursor::new(pem)))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parse PEM certificate chain")
}

fn pem_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut BufReader::new(Cursor::new(pem)))
        .context("parse PEM private key")?
        .ok_or_else(|| anyhow!("PEM contains no supported private key"))
}

fn normalize_serial(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;
    use orchestrator_legacy::NodeRecord;
    use orchestrator_storage::{NodeEnrollmentCode, SqliteOrchestratorStore};
    use rcgen::{BasicConstraints, CertificateParams};
    use tempfile::tempdir;

    struct Fixture {
        service: NodeIdentityService,
        csr_pem: String,
    }

    fn fixture() -> Fixture {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "OJOS test Node CA");
        let ca_key = KeyPair::generate().unwrap();
        let ca_certificate = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::from_params(&ca_params, &ca_key);

        let mut server_params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let server_key = KeyPair::generate().unwrap();
        let server_certificate = server_params.signed_by(&server_key, &issuer).unwrap();

        let node_key = KeyPair::generate().unwrap();
        let mut request_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        request_params
            .distinguished_name
            .push(DnType::CommonName, "untrusted CSR name");
        request_params.subject_alt_names = vec![SanType::URI(
            "spiffe://attacker.invalid/node/admin".try_into().unwrap(),
        )];
        let csr_pem = request_params
            .serialize_request(&node_key)
            .unwrap()
            .pem()
            .unwrap();
        let service = NodeIdentityService::from_pem(
            server_certificate.pem().as_bytes(),
            server_key.serialize_pem().as_bytes(),
            &ca_certificate.pem(),
            &ca_key.serialize_pem(),
        )
        .unwrap();
        Fixture { service, csr_pem }
    }

    #[test]
    fn production_configuration_fails_closed_without_tls_and_node_ca() {
        let mut environment = TestEnv::lock();
        for name in [TLS_CERT_ENV, TLS_KEY_ENV, NODE_CA_CERT_ENV, NODE_CA_KEY_ENV] {
            environment.remove(name);
        }
        let error = NodeIdentityService::from_env(true).unwrap_err();
        assert!(error.to_string().contains("requires ORCHESTRATOR_TLS_CERT"));
    }

    fn peer_from_pem(pem: &str) -> NodePeerIdentity {
        let certificate = pem_certificates(pem.as_bytes()).unwrap().remove(0);
        NodePeerIdentity::from_certificate_der(certificate.as_ref()).unwrap()
    }

    fn durable_pending_node(now_ms: i64) -> (tempfile::TempDir, DurableStore, String) {
        let directory = tempdir().unwrap();
        let durable = DurableStore::Sqlite(
            SqliteOrchestratorStore::open(directory.path().join("identity.db")).unwrap(),
        );
        let secret = "ojos_enroll_test-secret".to_string();
        let node = NodeRecord {
            node_id: "node-1".into(),
            host_ip: "127.0.0.2".into(),
            parent_node_id: String::new(),
            role: "standalone".into(),
            labels: serde_json::json!({}),
            status: "ENROLLMENT_PENDING".into(),
            created_at: format!("unix-ms:{now_ms}"),
            updated_at: format!("unix-ms:{now_ms}"),
        };
        durable
            .register_node_enrollment(
                &node,
                &NodeEnrollmentCode {
                    code_id: "code-1".into(),
                    secret_sha256: secret_digest(&secret),
                    node_id: node.node_id.clone(),
                    created_at_ms: now_ms,
                    expires_at_ms: now_ms + 60_000,
                    redeemed_at_ms: None,
                },
            )
            .unwrap();
        (directory, durable, secret)
    }

    #[test]
    fn issuer_replaces_all_csr_identity_fields_with_exact_node_spiffe_identity() {
        let fixture = fixture();
        let issued = fixture
            .service
            .issue("node-1", &fixture.csr_pem, 1_700_000_000_000)
            .unwrap();
        assert_eq!(issued.spiffe_id, "spiffe://ojos.local/node/node-1");
        assert_eq!(
            issued.not_after_ms - issued.not_before_ms,
            CERTIFICATE_LIFETIME_MS
        );
        let peer = peer_from_pem(&issued.certificate_pem);
        assert_eq!(peer.node_id, "node-1");
        assert_eq!(peer.serial_hex, issued.serial_hex);
        assert_eq!(peer.fingerprint_sha256, issued.fingerprint_sha256);
    }

    #[test]
    fn redeemed_identity_authenticates_then_immediate_revocation_rejects_it() {
        let now = 1_700_000_000_000;
        let fixture = fixture();
        let (_directory, durable, secret) = durable_pending_node(now);
        let redeemed = redeem(&durable, &fixture.service, &secret, &fixture.csr_pem, now).unwrap();
        let EnrollmentRedemption::Redeemed(certificate) = redeemed else {
            panic!("expected redeemed certificate")
        };
        let peer = peer_from_pem(&certificate.certificate_pem);
        fixture.service.authenticate(&durable, &peer, now).unwrap();
        durable
            .revoke_node_certificates("node-1", now + 1, "operator revoked")
            .unwrap();
        assert!(matches!(
            fixture.service.authenticate(&durable, &peer, now + 1),
            Err(NodeAuthenticationError::Revoked)
        ));
        assert_eq!(
            durable.get_node("node-1").unwrap().unwrap().status,
            "AUTH_REVOKED"
        );
    }

    #[test]
    fn lost_enrollment_response_replays_exact_certificate_only_for_the_same_csr() {
        let now = 1_700_000_000_000;
        let different_csr = fixture().csr_pem;
        let fixture = fixture();
        let (_directory, durable, secret) = durable_pending_node(now);
        let EnrollmentRedemption::Redeemed(original) =
            redeem(&durable, &fixture.service, &secret, &fixture.csr_pem, now).unwrap()
        else {
            panic!("expected first redemption")
        };
        let EnrollmentRedemption::Replayed(replayed) =
            redeem_with_issuer(&durable, &secret, &fixture.csr_pem, now + 1, |_, _, _| {
                panic!("same-CSR replay must not invoke the CA signer")
            })
            .unwrap()
        else {
            panic!("the same CSR must replay its committed certificate")
        };
        assert_eq!(replayed, original);
        assert!(matches!(
            redeem_with_issuer(&durable, &secret, &different_csr, now + 2, |_, _, _| {
                panic!("different-CSR replay must not invoke the CA signer")
            })
            .unwrap(),
            EnrollmentRedemption::AlreadyRedeemed
        ));
    }

    #[test]
    fn enrollment_replay_rejects_a_revoked_or_inactive_committed_certificate() {
        let now = 1_700_000_000_000;
        let fixture = fixture();
        let (_directory, durable, secret) = durable_pending_node(now);
        let EnrollmentRedemption::Redeemed(original) =
            redeem(&durable, &fixture.service, &secret, &fixture.csr_pem, now).unwrap()
        else {
            panic!("expected first redemption")
        };

        assert!(matches!(
            redeem_with_issuer(
                &durable,
                &secret,
                &fixture.csr_pem,
                original.not_before_ms - 1,
                |_, _, _| panic!("an inactive replay must not invoke the CA signer"),
            )
            .unwrap(),
            EnrollmentRedemption::ReplayCertificateNotYetValid
        ));
        assert!(matches!(
            redeem_with_issuer(
                &durable,
                &secret,
                &fixture.csr_pem,
                original.not_after_ms,
                |_, _, _| panic!("an expired replay must not invoke the CA signer"),
            )
            .unwrap(),
            EnrollmentRedemption::ReplayCertificateExpired
        ));

        durable
            .revoke_node_certificates("node-1", now + 1, "operator revoked")
            .unwrap();
        assert!(matches!(
            redeem_with_issuer(
                &durable,
                &secret,
                &fixture.csr_pem,
                now + 2,
                |_, _, _| panic!("a revoked replay must not invoke the CA signer"),
            )
            .unwrap(),
            EnrollmentRedemption::ReplayCertificateRevoked
        ));
    }

    #[test]
    fn renewal_keeps_old_identity_until_the_persisted_replacement_is_activated() {
        let now = 1_700_000_000_000;
        let fixture = fixture();
        let (_directory, durable, secret) = durable_pending_node(now);
        let EnrollmentRedemption::Redeemed(original) =
            redeem(&durable, &fixture.service, &secret, &fixture.csr_pem, now).unwrap()
        else {
            panic!("expected redeemed certificate")
        };
        let original_peer = peer_from_pem(&original.certificate_pem);
        let renewal_time =
            original.not_after_ms - orchestrator_storage::CERTIFICATE_RENEWAL_WINDOW_MS;
        let CertificateRotation::Rotated(replacement) = renew(
            &durable,
            &fixture.service,
            &original_peer,
            &fixture.csr_pem,
            renewal_time,
        )
        .unwrap() else {
            panic!("expected replacement certificate")
        };

        // Losing the response is recoverable because the old credential is
        // not revoked until the replacement proves possession.
        fixture
            .service
            .authenticate(&durable, &original_peer, renewal_time + 1)
            .unwrap();
        let replacement_peer = peer_from_pem(&replacement.certificate_pem);
        fixture
            .service
            .authenticate(&durable, &replacement_peer, renewal_time + 1)
            .unwrap();
        assert!(matches!(
            durable
                .activate_node_certificate("node-1", &replacement_peer.serial_hex, renewal_time + 1)
                .unwrap(),
            orchestrator_storage::CertificateActivation::Activated {
                revoked_certificates: 1
            }
        ));
        assert!(matches!(
            fixture
                .service
                .authenticate(&durable, &original_peer, renewal_time + 2),
            Err(NodeAuthenticationError::Revoked)
        ));
        fixture
            .service
            .authenticate(&durable, &replacement_peer, renewal_time + 2)
            .unwrap();
    }
}
