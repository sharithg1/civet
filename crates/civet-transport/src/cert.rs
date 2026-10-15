//! Daemon TLS identity (self-signed cert) and client-side fingerprint pinning.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use civet_proto::CertFingerprint;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::TransportError;

/// Subject alternative name of the daemon certificate and the SNI the client
/// sends.
pub const SERVER_NAME: &str = "civetd";

const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

/// Daemon TLS identity: self-signed ECDSA P-256 cert (rcgen), subject alt
/// name "civetd", validity 10 years.
pub struct ServerIdentity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl fmt::Debug for ServerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerIdentity")
            .field("fingerprint", &self.fingerprint())
            .field("key", &"<redacted>")
            .finish()
    }
}

impl Clone for ServerIdentity {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
        }
    }
}

impl ServerIdentity {
    /// Generate a fresh self-signed identity.
    pub fn generate() -> Result<Self, TransportError> {
        let tls = |e: rcgen::Error| TransportError::Tls(e.to_string());
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(tls)?;
        let mut params =
            rcgen::CertificateParams::new(vec![SERVER_NAME.to_string()]).map_err(tls)?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, SERVER_NAME);
        let year = current_year();
        params.not_before = rcgen::date_time_ymd(year - 1, 1, 1);
        params.not_after = rcgen::date_time_ymd(year + 10, 1, 1);
        let cert = params.self_signed(&key_pair).map_err(tls)?;
        Ok(Self {
            cert: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key_pair.serialize_der()),
        })
    }

    /// Load `<dir>/cert.der` + `<dir>/key.der`, or generate and write them
    /// (key file mode 0600, written via temp file + rename). Keeps the
    /// fingerprint stable across daemon restarts.
    pub fn load_or_generate(dir: &Path) -> Result<Self, TransportError> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.exists() && key_path.exists() {
            let cert = std::fs::read(&cert_path)?;
            let key = std::fs::read(&key_path)?;
            let id = Self {
                cert: CertificateDer::from(cert),
                key: PrivatePkcs8KeyDer::from(key),
            };
            // Validate the pair by building a server config with it.
            match crate::quic::server_crypto(&id) {
                Ok(_) => return Ok(id),
                Err(e) => {
                    tracing::warn!(error = %e, "stored TLS identity unusable; regenerating");
                }
            }
        }
        std::fs::create_dir_all(dir)?;
        let id = Self::generate()?;
        write_private(&key_path, id.key.secret_pkcs8_der())?;
        write_private(&cert_path, id.cert.as_ref())?;
        Ok(id)
    }

    /// SHA-256 of the certificate DER.
    pub fn fingerprint(&self) -> CertFingerprint {
        fingerprint_of(&self.cert)
    }

    /// Certificate DER bytes.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert
    }

    pub(crate) fn cert(&self) -> CertificateDer<'static> {
        self.cert.clone()
    }

    /// PKCS#8 DER of the private key (secret; never log it).
    pub(crate) fn key_der(&self) -> &[u8] {
        self.key.secret_pkcs8_der()
    }

    pub(crate) fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(self.key.clone_key())
    }
}

/// SHA-256 over the DER bytes of a certificate.
pub fn fingerprint_of(cert_der: &[u8]) -> CertFingerprint {
    let digest = Sha256::digest(cert_der);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    CertFingerprint(out)
}

fn current_year() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Average Gregorian year; exact day boundaries do not matter here.
    1970 + (secs / 31_556_952) as i32
}

/// Write `data` to `path` with mode 0600 via a temp file + rename.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts the end-entity certificate iff its SHA-256 equals the pinned
/// fingerprint; ignores names and chains, but verifies handshake signatures.
#[derive(Debug)]
pub(crate) struct PinnedVerifier {
    expected: CertFingerprint,
    provider: Arc<CryptoProvider>,
    /// Fingerprint actually presented when it did not match.
    mismatch: Arc<Mutex<Option<CertFingerprint>>>,
}

impl PinnedVerifier {
    pub(crate) fn new(
        expected: CertFingerprint,
        provider: Arc<CryptoProvider>,
    ) -> (Self, Arc<Mutex<Option<CertFingerprint>>>) {
        let slot = Arc::new(Mutex::new(None));
        (
            Self {
                expected,
                provider,
                mismatch: slot.clone(),
            },
            slot,
        )
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = fingerprint_of(end_entity);
        if bool::from(actual.0.ct_eq(&self.expected.0)) {
            Ok(ServerCertVerified::assertion())
        } else {
            if let Ok(mut slot) = self.mismatch.lock() {
                *slot = Some(actual);
            }
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_fingerprint() {
        let a = ServerIdentity::generate().unwrap();
        let b = ServerIdentity::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), fingerprint_of(a.cert_der()));
        assert!(format!("{a:?}").contains("redacted"));
    }

    #[test]
    fn load_or_generate_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let a = ServerIdentity::load_or_generate(dir.path()).unwrap();
        let b = ServerIdentity::load_or_generate(dir.path()).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A corrupt key is replaced with a fresh identity.
        std::fs::write(dir.path().join(KEY_FILE), b"garbage").unwrap();
        let c = ServerIdentity::load_or_generate(dir.path()).unwrap();
        assert_ne!(c.fingerprint(), a.fingerprint());
    }
}
