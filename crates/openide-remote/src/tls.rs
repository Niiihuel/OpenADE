//! The PC's own TLS identity. No authority signs it: the phone pins the
//! certificate's SHA-256, which travels in the pairing code.

use crate::storage;
use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

pub struct Identity {
    certificate: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    /// The identity kept under `root/tls`, made the first time.
    pub fn load_or_create(root: &Path) -> Result<Self> {
        let directory = root.join("tls");
        storage::prepare_directory(&directory)?;
        let (cert_path, key_path) = (directory.join("cert.der"), directory.join("key.der"));
        if cert_path.exists() && key_path.exists() {
            return Ok(Self {
                certificate: storage::read_private(&cert_path, 64 * 1024)?,
                key: storage::read_private(&key_path, 64 * 1024)?,
            });
        }
        let generated = rcgen::generate_simple_self_signed(vec!["openade.local".to_owned()])
            .context("cannot create the TLS certificate")?;
        let identity = Self {
            certificate: generated.cert.der().to_vec(),
            key: generated.key_pair.serialize_der(),
        };
        storage::write_private(&key_path, &identity.key)?;
        storage::write_private(&cert_path, &identity.certificate)?;
        Ok(identity)
    }

    /// SHA-256 of the certificate, lowercase hex: what the phone pins.
    pub fn fingerprint(&self) -> String {
        format!("{:x}", Sha256::digest(&self.certificate))
    }

    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(self.certificate.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone())),
            )?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

/// Four groups of the fingerprint, for a person to compare by eye.
pub fn short_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(4)
        .take(4)
        .map(|chunk| String::from_utf8_lossy(chunk).to_uppercase())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_is_made_once_and_kept() {
        let root = tempfile::tempdir().unwrap();
        let first = Identity::load_or_create(root.path()).unwrap();
        let again = Identity::load_or_create(root.path()).unwrap();
        assert_eq!(first.fingerprint(), again.fingerprint());
        assert_eq!(first.fingerprint().len(), 64);
        assert!(first.server_config().is_ok());
        assert_eq!(short_fingerprint("abcd1234ef567890ffff"), "ABCD-1234-EF56-7890");
    }
}
