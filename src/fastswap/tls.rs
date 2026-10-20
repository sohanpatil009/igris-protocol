use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use rcgen::{CertificateParams, IsCa, KeyUsagePurpose, KeyPair};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const CERT_FILE: &str = "fastswap_cert.der";
const KEY_FILE: &str = "fastswap_key.der";

pub struct TlsConfig {
    pub server_config: Arc<rustls::ServerConfig>,
}

fn cert_dir() -> PathBuf {
    crate::fastswap::fastswap_certs_dir()
}
fn cert_der_path() -> PathBuf {
    cert_dir().join(CERT_FILE)
}
fn key_der_path() -> PathBuf {
    cert_dir().join(KEY_FILE)
}

pub fn get_or_create_tls_config() -> Result<TlsConfig> {
    // Install ring-based crypto provider (required once per process)
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (cert_der, key_der) = if cert_der_path().exists() && key_der_path().exists() {
        (fs::read(cert_der_path())?, fs::read(key_der_path())?)
    } else {
        let mut params = CertificateParams::new(vec!["igris.local".to_string()])
            .context("Failed to create cert params")?;
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![
            KeyUsagePurpose::KeyEncipherment,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key_pair = KeyPair::generate().context("Failed to generate key pair")?;
        let cert = params.self_signed(&key_pair)
            .context("Failed to self-sign certificate")?;

        let cert_bytes = cert.der().to_vec();
        let key_bytes = key_pair.serialize_der();

        fs::create_dir_all(cert_dir()).context("Failed to create cert directory")?;
        fs::write(cert_der_path(), &cert_bytes)
            .context("Failed to write certificate")?;
        fs::write(key_der_path(), &key_bytes)
            .context("Failed to write key")?;

        (cert_bytes, key_bytes)
    };

    let cert = rustls::pki_types::CertificateDer::from(cert_der);
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(key_der),
    );

    let server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("Failed to build TLS server config")?;

    Ok(TlsConfig {
        server_config: Arc::new(server_config),
    })
}

/// Stores trusted peer certificate fingerprints (SHA-256 of DER bytes).
static TRUSTED_FINGERPRINTS: Lazy<Mutex<Vec<Vec<u8>>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Register a trusted peer fingerprint (called during pairing).
pub fn trust_peer_fingerprint(fingerprint: Vec<u8>) {
    if let Ok(mut fps) = TRUSTED_FINGERPRINTS.lock() {
        if !fps.contains(&fingerprint) {
            fps.push(fingerprint);
        }
    }
}

/// Compute SHA-256 fingerprint of a DER-encoded certificate.
fn cert_fingerprint(cert: &rustls::pki_types::CertificateDer<'_>) -> Vec<u8> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(cert.as_ref());
    hasher.finalize().to_vec()
}

pub fn get_tofu_client_config() -> Result<rustls::ClientConfig> {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(TofuCertVerifier))
        .with_no_client_auth();
    Ok(config)
}

#[derive(Debug)]
struct TofuCertVerifier;

impl rustls::client::danger::ServerCertVerifier for TofuCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = cert_fingerprint(end_entity);
        let trusted = TRUSTED_FINGERPRINTS.lock().ok();

        if let Some(ref fps) = trusted {
            if fps.is_empty() {
                // First connection — trust on first use
                drop(trusted);
                trust_peer_fingerprint(fp);
                return Ok(rustls::client::danger::ServerCertVerified::assertion());
            }
            if fps.contains(&fp) {
                return Ok(rustls::client::danger::ServerCertVerified::assertion());
            }
        }

        // Unknown fingerprint — reject
        Err(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
        ]
    }
}
