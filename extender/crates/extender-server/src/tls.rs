//! TLS configuration and acceptor setup for the USB/IP server.
//!
//! Provides [`TlsServerConfig`] for loading certificates and private keys,
//! and building a [`tokio_rustls::TlsAcceptor`].

use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use crate::error::ServerError;

/// TLS configuration for the server.
#[derive(Debug, Clone)]
pub struct TlsServerConfig {
    /// Path to the PEM-encoded certificate chain file.
    pub cert_path: String,
    /// Path to the PEM-encoded private key file.
    pub key_path: String,
    /// Optional path to a CA certificate for client verification (mTLS).
    pub ca_path: Option<String>,
}

impl TlsServerConfig {
    /// Build a [`TlsAcceptor`] from the configured certificate and key files.
    pub fn build_acceptor(&self) -> Result<TlsAcceptor, ServerError> {
        let certs = load_certs(&self.cert_path)?;
        let key = load_private_key(&self.key_path)?;

        let mut server_config = if let Some(ca_path) = &self.ca_path {
            // mTLS: require client certificates signed by the given CA.
            let ca_certs = load_certs(ca_path)?;
            let mut root_store = rustls::RootCertStore::empty();
            for cert in ca_certs {
                root_store
                    .add(cert)
                    .map_err(|e| ServerError::Tls(format!("failed to add CA certificate: {e}")))?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
                .build()
                .map_err(|e| ServerError::Tls(format!("failed to build client verifier: {e}")))?;
            rustls::ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(certs, key)
                .map_err(|e| ServerError::Tls(format!("invalid server TLS config: {e}")))?
        } else {
            // Standard TLS: no client certificate required.
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .map_err(|e| ServerError::Tls(format!("invalid server TLS config: {e}")))?
        };

        server_config.alpn_protocols = vec![b"usbip".to_vec()];

        Ok(TlsAcceptor::from(Arc::new(server_config)))
    }
}

/// Load PEM-encoded certificates from a file.
fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>, ServerError> {
    let file = fs::File::open(Path::new(path))
        .map_err(|e| ServerError::Tls(format!("failed to open cert file {path}: {e}")))?;
    let mut reader = BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ServerError::Tls(format!("failed to parse certs from {path}: {e}")))?;
    if certs.is_empty() {
        return Err(ServerError::Tls(format!("no certificates found in {path}")));
    }
    Ok(certs)
}

/// Load a PEM-encoded private key from a file.
fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>, ServerError> {
    let file = fs::File::open(Path::new(path))
        .map_err(|e| ServerError::Tls(format!("failed to open key file {path}: {e}")))?;
    let mut reader = BufReader::new(file);

    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|e| ServerError::Tls(format!("failed to parse key from {path}: {e}")))?
        {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => {
                return Ok(PrivateKeyDer::Pkcs1(key));
            }
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => {
                return Ok(PrivateKeyDer::Pkcs8(key));
            }
            Some(rustls_pemfile::Item::Sec1Key(key)) => {
                return Ok(PrivateKeyDer::Sec1(key));
            }
            None => break,
            _ => continue,
        }
    }

    Err(ServerError::Tls(format!("no private key found in {path}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_certs_missing_file() {
        let result = load_certs("/nonexistent/cert.pem");
        assert!(result.is_err());
    }

    #[test]
    fn test_load_key_missing_file() {
        let result = load_private_key("/nonexistent/key.pem");
        assert!(result.is_err());
    }
}
