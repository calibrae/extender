//! `extender daemon` subcommand — starts the daemon in the foreground.

use extender_daemon::config::Config;
use extender_daemon::Daemon;

/// Run the daemon in the foreground with the given CLI overrides.
pub async fn run(
    port: Option<u16>,
    listen: Option<&str>,
    socket: Option<&str>,
    config_path: Option<&str>,
    log_level: Option<&str>,
    tls_cert: Option<&str>,
    tls_key: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load configuration: from file if specified, otherwise default chain.
    let mut config = if let Some(path) = config_path {
        Config::load_file(path).unwrap_or_else(|| {
            eprintln!("warning: could not load config from {path}, using defaults");
            Config::load()
        })
    } else {
        Config::load()
    };

    // Apply CLI overrides.
    if let Some(p) = port {
        config.server.port = p;
    }
    if let Some(addr) = listen {
        config.server.listen_address = addr.to_string();
    }
    if let Some(sock) = socket {
        config.daemon.socket_path = sock.to_string();
    }
    if let Some(level) = log_level {
        config.daemon.log_level = level.to_string();
    }
    if let Some(cert) = tls_cert {
        config.server.tls_cert = Some(cert.to_string());
    }
    if let Some(key) = tls_key {
        config.server.tls_key = Some(key.to_string());
    }

    // Warn if listening on a non-localhost address without TLS.
    let listen_addr = config.server.listen_address.as_str();
    let has_tls = config.server.tls_cert.is_some() && config.server.tls_key.is_some();
    if listen_addr != "127.0.0.1" && listen_addr != "::1" && listen_addr != "localhost" && !has_tls
    {
        eprintln!("WARNING: USB/IP server listening on non-localhost address without TLS. Traffic is unencrypted. Use --tls-cert/--tls-key, a VPN, or SSH tunnel for security.");
    }

    let daemon = Daemon::new(config);
    daemon.init_logging();
    daemon.run().await
}
