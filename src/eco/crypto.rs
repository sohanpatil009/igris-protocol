use crate::eco::errors::{EcoError, EcoResult};
use std::path::PathBuf;

const KEY_DIR: &str = "ecosystem";
const PRIVATE_KEY_FILE: &str = "ecosystem_key.pem";
const PUBLIC_KEY_FILE: &str = "ecosystem_key.pub";

pub struct EcoCrypto {
    key_pair: rcgen::KeyPair,
    certificate: rcgen::Certificate,
}

impl EcoCrypto {
    pub fn new(pkg_dir: &PathBuf) -> EcoResult<Self> {
        let key_dir = pkg_dir.join(KEY_DIR);
        std::fs::create_dir_all(&key_dir).map_err(EcoError::Io)?;

        let private_key_path = key_dir.join(PRIVATE_KEY_FILE);
        let public_key_path = key_dir.join(PUBLIC_KEY_FILE);

        if private_key_path.exists() {
            let pem = std::fs::read_to_string(&private_key_path)
                .map_err(EcoError::Io)?;
            let key_pair = rcgen::KeyPair::from_pem(&pem)
                .map_err(|e| EcoError::Crypto(e.to_string()))?;

            let params = rcgen::CertificateParams::new(vec!["ecosystem.local".to_string()])
                .map_err(|e| EcoError::Crypto(e.to_string()))?;
            let certificate = params.self_signed(&key_pair)
                .map_err(|e| EcoError::Crypto(e.to_string()))?;

            Ok(Self { key_pair, certificate })
        } else {
            let key_pair = rcgen::KeyPair::generate()
                .map_err(|e| EcoError::Crypto(e.to_string()))?;

            let params = rcgen::CertificateParams::new(vec!["ecosystem.local".to_string()])
                .map_err(|e| EcoError::Crypto(e.to_string()))?;
            let certificate = params.self_signed(&key_pair)
                .map_err(|e| EcoError::Crypto(e.to_string()))?;

            let private_pem = key_pair.serialize_pem();
            let public_der = certificate.der().to_vec();

            std::fs::write(&private_key_path, &private_pem)
                .map_err(EcoError::Io)?;
            std::fs::write(&public_key_path, &public_der)
                .map_err(EcoError::Io)?;

            Ok(Self { key_pair, certificate })
        }
    }

    pub fn public_key_der(&self) -> Vec<u8> {
        self.certificate.der().to_vec()
    }

    pub fn public_key_pem(&self) -> String {
        self.certificate.pem()
    }

    pub fn key_pair(&self) -> &rcgen::KeyPair {
        &self.key_pair
    }
}
