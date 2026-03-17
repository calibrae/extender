//! `extender tls-gen` subcommand — generate self-signed TLS certificates.
//!
//! Generates a self-signed CA certificate, a server certificate signed by
//! the CA, and a client certificate signed by the CA. All output as PEM files.

use std::fs;
use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};

/// Default output directory: `~/.config/extender/tls/`.
fn default_output_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("extender")
        .join("tls")
}

/// Execute the tls-gen command.
pub fn run(output_dir: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let dir = match output_dir {
        Some(d) => PathBuf::from(d),
        None => default_output_dir(),
    };

    fs::create_dir_all(&dir)?;

    eprintln!("Generating TLS certificates in {}", dir.display());

    // 1. Generate CA key pair and self-signed CA certificate.
    let ca_key_pair = KeyPair::generate()?;

    let mut ca_params = CertificateParams::new(vec!["Extender CA".to_string()])?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Extender CA");
    ca_params
        .distinguished_name
        .push(DnType::OrganizationName, "Extender");
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

    let ca_cert = ca_params.self_signed(&ca_key_pair)?;

    write_pem(&dir.join("ca.pem"), &ca_cert.pem())?;
    write_pem(&dir.join("ca-key.pem"), &ca_key_pair.serialize_pem())?;

    // 2. Generate server key pair and certificate signed by the CA.
    let server_key_pair = KeyPair::generate()?;

    let mut server_params =
        CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
    server_params
        .distinguished_name
        .push(DnType::CommonName, "Extender Server");
    server_params
        .distinguished_name
        .push(DnType::OrganizationName, "Extender");
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let server_cert = server_params.signed_by(&server_key_pair, &ca_cert, &ca_key_pair)?;

    write_pem(&dir.join("server-cert.pem"), &server_cert.pem())?;
    write_pem(
        &dir.join("server-key.pem"),
        &server_key_pair.serialize_pem(),
    )?;

    // 3. Generate client key pair and certificate signed by the CA.
    let client_key_pair = KeyPair::generate()?;

    let mut client_params = CertificateParams::new(vec!["extender-client".to_string()])?;
    client_params
        .distinguished_name
        .push(DnType::CommonName, "Extender Client");
    client_params
        .distinguished_name
        .push(DnType::OrganizationName, "Extender");
    client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];

    let client_cert = client_params.signed_by(&client_key_pair, &ca_cert, &ca_key_pair)?;

    write_pem(&dir.join("client-cert.pem"), &client_cert.pem())?;
    write_pem(
        &dir.join("client-key.pem"),
        &client_key_pair.serialize_pem(),
    )?;

    eprintln!("Generated files:");
    eprintln!("  CA certificate:     {}/ca.pem", dir.display());
    eprintln!("  CA private key:     {}/ca-key.pem", dir.display());
    eprintln!("  Server certificate: {}/server-cert.pem", dir.display());
    eprintln!("  Server private key: {}/server-key.pem", dir.display());
    eprintln!("  Client certificate: {}/client-cert.pem", dir.display());
    eprintln!("  Client private key: {}/client-key.pem", dir.display());
    eprintln!();
    eprintln!("Usage:");
    eprintln!(
        "  Server: extender daemon --tls-cert {0}/server-cert.pem --tls-key {0}/server-key.pem",
        dir.display()
    );
    eprintln!(
        "  Client: extender list -r <host> --tls --tls-ca {}/ca.pem",
        dir.display()
    );

    Ok(())
}

/// Write PEM content to a file, setting restrictive permissions.
fn write_pem(path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, content)?;

    // Best-effort: set restrictive permissions on key files.
    #[cfg(unix)]
    if path
        .file_name()
        .map(|n| n.to_string_lossy().contains("key"))
        .unwrap_or(false)
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }

    Ok(())
}
