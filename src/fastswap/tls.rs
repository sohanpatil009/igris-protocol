use anyhow::{Context, Result};
use rcgen::{CertificateParams, IsCa, BasicConstraints, KeyUsagePurpose, KeyPair};
use rustls::SignatureScheme;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

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
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
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

pub fn get_dangerous_client_config() -> Result<rustls::ClientConfig> {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(
            Arc::new(AcceptAnyCertVerifier),
        )
        .with_no_client_auth();
    Ok(config)
}

#[derive(Debug)]
struct AcceptAnyCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
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
