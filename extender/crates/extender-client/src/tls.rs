//! TLS configuration for the USB/IP client.
//!
//! Provides [`TlsClientConfig`] for creating a [`tokio_rustls::TlsConnector`]
//! used to wrap outgoing TCP connections with TLS.

use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::TlsConnector;

use crate::error::ClientError;

/// TLS configuration for outgoing client connections.
#[derive(Debug, Clone)]
pub struct TlsClientConfig {
    /// Optional path to a CA certificate to trust (instead of system roots).
    pub ca_path: Option<String>,
    /// Optional path to a client certificate (for mTLS).
    pub client_cert_path: Option<String>,
    /// Optional path to the client private key (for mTLS).
    pub client_key_path: Option<String>,
    /// Server name for SNI / certificate verification.
    /// If `None`, defaults to "localhost".
    pub server_name: Option<String>,
}

impl TlsClientConfig {
    /// Create a minimal TLS client config that trusts the given CA.
    pub fn with_ca(ca_path: &str) -> Self {
        TlsClientConfig {
            ca_path: Some(ca_path.to_owned()),
            client_cert_path: None,
            client_key_path: None,
            server_name: None,
        }
    }

    /// Build a [`TlsConnector`] from the configuration.
    pub fn build_connector(&self) -> Result<TlsConnector, ClientError> {
        let mut root_store = rustls::RootCertStore::empty();

        if let Some(ca_path) = &self.ca_path {
            let certs = load_certs(ca_path)?;
            for cert in certs {
                root_store
                    .add(cert)
                    .map_err(|e| ClientError::Tls(format!("failed to add CA certificate: {e}")))?;
            }
        } else {
            // No custom CA: use an empty root store.
            // In production, you might want to load system roots here.
        }

        let builder = rustls::ClientConfig::builder().with_root_certificates(root_store);

        let client_config = if let (Some(cert_path), Some(key_path)) =
            (&self.client_cert_path, &self.client_key_path)
        {
            let certs = load_certs(cert_path)?;
            let key = load_private_key(key_path)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| ClientError::Tls(format!("invalid client auth config: {e}")))?
        } else {
            builder.with_no_client_auth()
        };

        Ok(TlsConnector::from(Arc::new(client_config)))
    }

    /// Get the [`ServerName`] to use for TLS verification.
    pub fn server_name(&self) -> Result<ServerName<'static>, ClientError> {
        let name = self.server_name.as_deref().unwrap_or("localhost");
        ServerName::try_from(name.to_owned())
            .map_err(|e| ClientError::Tls(format!("invalid server name '{name}': {e}")))
    }
}

/// Load PEM-encoded certificates from a file.
fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>, ClientError> {
    let file = fs::File::open(Path::new(path))
        .map_err(|e| ClientError::Tls(format!("failed to open cert file {path}: {e}")))?;
    let mut reader = BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ClientError::Tls(format!("failed to parse certs from {path}: {e}")))?;
    if certs.is_empty() {
        return Err(ClientError::Tls(format!("no certificates found in {path}")));
    }
    Ok(certs)
}

/// Load a PEM-encoded private key from a file.
fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>, ClientError> {
    let file = fs::File::open(Path::new(path))
        .map_err(|e| ClientError::Tls(format!("failed to open key file {path}: {e}")))?;
    let mut reader = BufReader::new(file);

    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|e| ClientError::Tls(format!("failed to parse key from {path}: {e}")))?
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

    Err(ClientError::Tls(format!("no private key found in {path}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_client_config_with_ca() {
        let config = TlsClientConfig::with_ca("/some/path/ca.pem");
        assert_eq!(config.ca_path.as_deref(), Some("/some/path/ca.pem"));
        assert!(config.client_cert_path.is_none());
        assert!(config.client_key_path.is_none());
    }

    #[test]
    fn test_server_name_default() {
        let config = TlsClientConfig {
            ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            server_name: None,
        };
        let name = config.server_name().unwrap();
        assert_eq!(format!("{name:?}"), "DnsName(\"localhost\")");
    }

    #[test]
    fn test_server_name_custom() {
        let config = TlsClientConfig {
            ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            server_name: Some("myserver.local".to_string()),
        };
        let name = config.server_name().unwrap();
        assert_eq!(format!("{name:?}"), "DnsName(\"myserver.local\")");
    }
}
