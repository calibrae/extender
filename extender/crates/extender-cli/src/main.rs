//! Extender CLI — thin client over the daemon's Unix socket JSON-RPC API.

mod client;
mod commands;
mod output;

use clap::{Parser, Subcommand};
use output::OutputFormat;

/// Compute the default socket path (same logic as the daemon).
fn default_socket_path() -> String {
    if nix::unistd::geteuid().is_root() {
        "/var/run/extender.sock".to_string()
    } else {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        format!("{}/extender.sock", runtime_dir)
    }
}

/// Extender — USB/IP device sharing.
#[derive(Debug, Parser)]
#[command(
    name = "extender",
    version,
    about = "USB/IP device sharing over TCP/IP"
)]
pub struct Cli {
    /// Path to the daemon Unix socket.
    #[arg(long, global = true, default_value_t = default_socket_path())]
    socket: String,

    /// Output format.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List USB devices (local or remote).
    List {
        /// List local USB devices.
        #[arg(short, long)]
        local: bool,

        /// Query a remote server for its exported devices.
        #[arg(short, long)]
        remote: Option<String>,

        /// Remote server USB/IP port (default: 3240).
        #[arg(short, long)]
        port: Option<u16>,

        /// Enable TLS for the remote connection.
        #[arg(long)]
        tls: bool,

        /// Path to CA certificate for TLS verification.
        #[arg(long)]
        tls_ca: Option<String>,
    },

    /// Export a local USB device for remote access.
    Bind {
        /// Bus ID of the device to export (e.g. "1-1").
        #[arg(short, long)]
        busid: String,
    },

    /// Stop exporting a local USB device.
    Unbind {
        /// Bus ID of the device to unexport.
        #[arg(short, long)]
        busid: String,
    },

    /// Import a remote USB device.
    Attach {
        /// Remote server hostname or IP.
        #[arg(short, long)]
        remote: String,

        /// Bus ID of the remote device to attach.
        #[arg(short, long)]
        busid: String,

        /// Remote server USB/IP port (default: 3240).
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Detach an imported USB device.
    Detach {
        /// Virtual HCI port number of the device to detach.
        #[arg(short, long)]
        port: u32,
    },

    /// Discover Extender servers on the LAN via mDNS/DNS-SD.
    Discover {
        /// Discovery timeout in seconds (default: 3).
        #[arg(short, long)]
        timeout: Option<u64>,
    },

    /// Show daemon status and device overview.
    Status,

    /// Start the daemon in the foreground.
    Daemon {
        /// USB/IP listen port.
        #[arg(short, long)]
        port: Option<u16>,

        /// Listen address for USB/IP connections.
        #[arg(short, long)]
        listen: Option<String>,

        /// API socket path override.
        #[arg(long = "api-socket")]
        api_socket: Option<String>,

        /// Path to configuration file.
        #[arg(short, long)]
        config: Option<String>,

        /// Log verbosity level (trace, debug, info, warn, error).
        #[arg(long)]
        log_level: Option<String>,

        /// Path to PEM certificate file for TLS.
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to PEM private key file for TLS.
        #[arg(long)]
        tls_key: Option<String>,
    },

    /// Generate self-signed TLS certificates for the server and client.
    TlsGen {
        /// Output directory (default: ~/.config/extender/tls/).
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Print version information.
    Version,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = run(cli).await;

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::List {
            local,
            remote,
            port,
            tls,
            tls_ca,
        } => {
            commands::list::run(
                &cli.socket,
                cli.format,
                local,
                remote.as_deref(),
                port,
                tls,
                tls_ca.as_deref(),
            )
            .await
        }

        Command::Bind { busid } => commands::bind::run_bind(&cli.socket, cli.format, &busid).await,

        Command::Unbind { busid } => {
            commands::bind::run_unbind(&cli.socket, cli.format, &busid).await
        }

        Command::Attach {
            remote,
            busid,
            port,
        } => commands::attach::run_attach(&cli.socket, cli.format, &remote, &busid, port).await,

        Command::Detach { port } => {
            commands::attach::run_detach(&cli.socket, cli.format, port).await
        }

        Command::Discover { timeout } => commands::discover::run(cli.format, timeout).await,

        Command::Status => commands::status::run(&cli.socket, cli.format).await,

        Command::Daemon {
            port,
            listen,
            api_socket,
            config,
            log_level,
            tls_cert,
            tls_key,
        } => {
            commands::daemon::run(
                port,
                listen.as_deref(),
                api_socket.as_deref(),
                config.as_deref(),
                log_level.as_deref(),
                tls_cert.as_deref(),
                tls_key.as_deref(),
            )
            .await
        }

        Command::TlsGen { output } => commands::tls_gen::run(output.as_deref()),

        Command::Version => {
            println!("extender {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_parse_version() {
        let cli = Cli::try_parse_from(["extender", "version"]).unwrap();
        assert!(matches!(cli.command, Command::Version));
    }

    #[test]
    fn test_parse_list_local() {
        let cli = Cli::try_parse_from(["extender", "list", "--local"]).unwrap();
        match cli.command {
            Command::List { local, remote, .. } => {
                assert!(local);
                assert!(remote.is_none());
            }
            _ => panic!("expected List command"),
        }
    }

    #[test]
    fn test_parse_list_remote() {
        let cli = Cli::try_parse_from(["extender", "list", "--remote", "192.168.1.10"]).unwrap();
        match cli.command {
            Command::List { remote, .. } => {
                assert_eq!(remote.as_deref(), Some("192.168.1.10"));
            }
            _ => panic!("expected List command"),
        }
    }

    #[test]
    fn test_parse_list_remote_with_port() {
        let cli = Cli::try_parse_from(["extender", "list", "--remote", "server", "--port", "9999"])
            .unwrap();
        match cli.command {
            Command::List { remote, port, .. } => {
                assert_eq!(remote.as_deref(), Some("server"));
                assert_eq!(port, Some(9999));
            }
            _ => panic!("expected List command"),
        }
    }

    #[test]
    fn test_parse_bind() {
        let cli = Cli::try_parse_from(["extender", "bind", "-b", "1-1"]).unwrap();
        match cli.command {
            Command::Bind { busid } => assert_eq!(busid, "1-1"),
            _ => panic!("expected Bind command"),
        }
    }

    #[test]
    fn test_parse_unbind() {
        let cli = Cli::try_parse_from(["extender", "unbind", "--busid", "2-3"]).unwrap();
        match cli.command {
            Command::Unbind { busid } => assert_eq!(busid, "2-3"),
            _ => panic!("expected Unbind command"),
        }
    }

    #[test]
    fn test_parse_attach() {
        let cli =
            Cli::try_parse_from(["extender", "attach", "-r", "10.0.0.1", "-b", "1-2"]).unwrap();
        match cli.command {
            Command::Attach {
                remote,
                busid,
                port,
            } => {
                assert_eq!(remote, "10.0.0.1");
                assert_eq!(busid, "1-2");
                assert!(port.is_none());
            }
            _ => panic!("expected Attach command"),
        }
    }

    #[test]
    fn test_parse_attach_with_port() {
        let cli = Cli::try_parse_from([
            "extender", "attach", "-r", "host", "-b", "3-1", "-p", "4000",
        ])
        .unwrap();
        match cli.command {
            Command::Attach { port, .. } => assert_eq!(port, Some(4000)),
            _ => panic!("expected Attach command"),
        }
    }

    #[test]
    fn test_parse_detach() {
        let cli = Cli::try_parse_from(["extender", "detach", "-p", "0"]).unwrap();
        match cli.command {
            Command::Detach { port } => assert_eq!(port, 0),
            _ => panic!("expected Detach command"),
        }
    }

    #[test]
    fn test_parse_status() {
        let cli = Cli::try_parse_from(["extender", "status"]).unwrap();
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn test_parse_daemon() {
        let cli = Cli::try_parse_from(["extender", "daemon"]).unwrap();
        match cli.command {
            Command::Daemon {
                port,
                listen,
                api_socket,
                config,
                log_level,
                tls_cert,
                tls_key,
            } => {
                assert!(port.is_none());
                assert!(listen.is_none());
                assert!(api_socket.is_none());
                assert!(config.is_none());
                assert!(log_level.is_none());
                assert!(tls_cert.is_none());
                assert!(tls_key.is_none());
            }
            _ => panic!("expected Daemon command"),
        }
    }

    #[test]
    fn test_parse_daemon_with_options() {
        let cli = Cli::try_parse_from([
            "extender",
            "daemon",
            "--port",
            "5000",
            "--listen",
            "0.0.0.0",
            "--api-socket",
            "/tmp/test.sock",
            "--config",
            "/etc/extender.toml",
            "--log-level",
            "debug",
        ])
        .unwrap();
        match cli.command {
            Command::Daemon {
                port,
                listen,
                api_socket,
                config,
                log_level,
                ..
            } => {
                assert_eq!(port, Some(5000));
                assert_eq!(listen.as_deref(), Some("0.0.0.0"));
                assert_eq!(api_socket.as_deref(), Some("/tmp/test.sock"));
                assert_eq!(config.as_deref(), Some("/etc/extender.toml"));
                assert_eq!(log_level.as_deref(), Some("debug"));
            }
            _ => panic!("expected Daemon command"),
        }
    }

    #[test]
    fn test_global_format_option() {
        let cli = Cli::try_parse_from(["extender", "--format", "json", "version"]).unwrap();
        assert_eq!(cli.format, OutputFormat::Json);
    }

    #[test]
    fn test_global_socket_option() {
        let cli =
            Cli::try_parse_from(["extender", "--socket", "/tmp/custom.sock", "status"]).unwrap();
        assert_eq!(cli.socket, "/tmp/custom.sock");
    }

    #[test]
    fn test_default_socket_path() {
        let cli = Cli::try_parse_from(["extender", "version"]).unwrap();
        // Default depends on whether running as root and XDG_RUNTIME_DIR
        assert!(
            cli.socket.ends_with("/extender.sock"),
            "socket path should end with /extender.sock, got: {}",
            cli.socket
        );
    }

    #[test]
    fn test_default_format() {
        let cli = Cli::try_parse_from(["extender", "version"]).unwrap();
        assert_eq!(cli.format, OutputFormat::Human);
    }

    #[test]
    fn test_parse_discover() {
        let cli = Cli::try_parse_from(["extender", "discover"]).unwrap();
        assert!(matches!(cli.command, Command::Discover { timeout: None }));
    }

    #[test]
    fn test_parse_discover_with_timeout() {
        let cli = Cli::try_parse_from(["extender", "discover", "--timeout", "5"]).unwrap();
        match cli.command {
            Command::Discover { timeout } => assert_eq!(timeout, Some(5)),
            _ => panic!("expected Discover command"),
        }
    }

    #[test]
    fn test_parse_daemon_with_tls() {
        let cli = Cli::try_parse_from([
            "extender",
            "daemon",
            "--tls-cert",
            "/path/to/cert.pem",
            "--tls-key",
            "/path/to/key.pem",
        ])
        .unwrap();
        match cli.command {
            Command::Daemon {
                tls_cert, tls_key, ..
            } => {
                assert_eq!(tls_cert.as_deref(), Some("/path/to/cert.pem"));
                assert_eq!(tls_key.as_deref(), Some("/path/to/key.pem"));
            }
            _ => panic!("expected Daemon command"),
        }
    }

    #[test]
    fn test_parse_list_with_tls() {
        let cli = Cli::try_parse_from([
            "extender",
            "list",
            "--remote",
            "server",
            "--tls",
            "--tls-ca",
            "/path/to/ca.pem",
        ])
        .unwrap();
        match cli.command {
            Command::List {
                remote,
                tls,
                tls_ca,
                ..
            } => {
                assert_eq!(remote.as_deref(), Some("server"));
                assert!(tls);
                assert_eq!(tls_ca.as_deref(), Some("/path/to/ca.pem"));
            }
            _ => panic!("expected List command"),
        }
    }

    #[test]
    fn test_parse_tls_gen() {
        let cli = Cli::try_parse_from(["extender", "tls-gen"]).unwrap();
        assert!(matches!(cli.command, Command::TlsGen { output: None }));
    }

    #[test]
    fn test_parse_tls_gen_with_output() {
        let cli = Cli::try_parse_from(["extender", "tls-gen", "--output", "/tmp/certs"]).unwrap();
        match cli.command {
            Command::TlsGen { output } => {
                assert_eq!(output.as_deref(), Some("/tmp/certs"));
            }
            _ => panic!("expected TlsGen command"),
        }
    }

    #[test]
    fn test_missing_subcommand_is_error() {
        let result = Cli::try_parse_from(["extender"]);
        assert!(result.is_err());
    }
}
