//! Integration tests for TLS-encrypted connections.
//!
//! Uses `rcgen` to generate certificates in-memory, writes them to temp files,
//! then starts a TLS server and verifies both TLS client connections and
//! plain TCP rejection.

use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};

use extender_protocol::codec::{read_op_message, write_op_message};
use extender_protocol::{OpMessage, OpReqDevlist};

use extender_server::engine::ServerEngine;
use extender_server::export::ExportRegistry;
use extender_server::tls::TlsServerConfig;

use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair};

/// Generate CA, server cert, and write them to temp files.
/// Returns (ca_cert_path, server_cert_path, server_key_path).
fn generate_test_certs() -> (
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
) {
    // Generate CA
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(vec!["Test CA".to_string()]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Test CA");
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    // Generate server cert signed by CA
    let server_key = KeyPair::generate().unwrap();
    let mut server_params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]).unwrap();
    server_params
        .distinguished_name
        .push(DnType::CommonName, "Test Server");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    // Write to temp files
    let mut ca_file = tempfile::NamedTempFile::new().unwrap();
    ca_file.write_all(ca_cert.pem().as_bytes()).unwrap();
    ca_file.flush().unwrap();

    let mut cert_file = tempfile::NamedTempFile::new().unwrap();
    cert_file.write_all(server_cert.pem().as_bytes()).unwrap();
    cert_file.flush().unwrap();

    let mut key_file = tempfile::NamedTempFile::new().unwrap();
    key_file
        .write_all(server_key.serialize_pem().as_bytes())
        .unwrap();
    key_file.flush().unwrap();

    (ca_file, cert_file, key_file)
}

/// Start a TLS-enabled test server.
async fn start_tls_test_server(
    cert_path: &str,
    key_path: &str,
) -> (SocketAddr, Arc<ExportRegistry>) {
    let tls_config = TlsServerConfig {
        cert_path: cert_path.to_string(),
        key_path: key_path.to_string(),
        ca_path: None,
    };
    let registry = Arc::new(ExportRegistry::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let acceptor = tls_config.build_acceptor().unwrap();
    let engine = ServerEngine::from_parts_tls(listener, Arc::clone(&registry), Some(acceptor));
    assert!(engine.tls_enabled());

    tokio::spawn(async move {
        engine.run().await.ok();
    });

    // Give the server a moment to start accepting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (addr, registry)
}

#[tokio::test]
async fn test_tls_devlist_exchange() {
    let (ca_file, cert_file, key_file) = generate_test_certs();

    let (addr, _registry) = start_tls_test_server(
        cert_file.path().to_str().unwrap(),
        key_file.path().to_str().unwrap(),
    )
    .await;

    // Connect with TLS client
    let tcp_stream = TcpStream::connect(addr).await.unwrap();

    // Build TLS client config trusting our test CA
    let ca_pem = std::fs::read(ca_file.path()).unwrap();
    let mut root_store = rustls::RootCertStore::empty();
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut &ca_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
    for cert in certs {
        root_store.add(cert).unwrap();
    }

    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));

    let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls_stream = connector.connect(server_name, tcp_stream).await.unwrap();

    let (mut reader, mut writer) = tokio::io::split(tls_stream);

    // Send DEVLIST request
    let req = OpMessage::ReqDevlist(OpReqDevlist);
    write_op_message(&mut writer, &req).await.unwrap();

    // Read DEVLIST reply
    let reply = read_op_message(&mut reader).await.unwrap();
    match reply {
        OpMessage::RepDevlist(rep) => {
            assert_eq!(rep.status, 0);
            assert!(rep.devices.is_empty());
        }
        other => panic!("expected RepDevlist, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_tls_server_rejects_plain_tcp() {
    let (_ca_file, cert_file, key_file) = generate_test_certs();

    let (addr, _registry) = start_tls_test_server(
        cert_file.path().to_str().unwrap(),
        key_file.path().to_str().unwrap(),
    )
    .await;

    // Connect with plain TCP (no TLS)
    let mut tcp_stream = TcpStream::connect(addr).await.unwrap();

    // Send DEVLIST request as plain TCP -- should fail because server expects TLS.
    let req = OpMessage::ReqDevlist(OpReqDevlist);
    let send_result = write_op_message(&mut tcp_stream, &req).await;

    // The write may succeed (data buffered), but reading the response should fail
    // because the server will reject the non-TLS connection.
    if send_result.is_ok() {
        let read_result = read_op_message(&mut tcp_stream).await;
        assert!(
            read_result.is_err(),
            "plain TCP connection should be rejected by TLS server"
        );
    }
    // If send failed, that's also an acceptable outcome.
}

#[tokio::test]
async fn test_tls_server_config_build() {
    let (_ca_file, cert_file, key_file) = generate_test_certs();

    let config = TlsServerConfig {
        cert_path: cert_file.path().to_str().unwrap().to_string(),
        key_path: key_file.path().to_str().unwrap().to_string(),
        ca_path: None,
    };

    let result = config.build_acceptor();
    assert!(result.is_ok(), "TLS acceptor should build successfully");
}

#[tokio::test]
async fn test_tls_server_config_bad_cert() {
    let config = TlsServerConfig {
        cert_path: "/nonexistent/cert.pem".to_string(),
        key_path: "/nonexistent/key.pem".to_string(),
        ca_path: None,
    };

    let result = config.build_acceptor();
    assert!(result.is_err(), "should fail with missing cert files");
}
