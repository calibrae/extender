//! Extender daemon: orchestrates server/client engines and serves the API.
//!
//! The daemon binds a Unix domain socket, listens for JSON-RPC requests from
//! the CLI (or other clients), and delegates to the server and client engines.

pub mod api_server;
pub mod config;
pub mod device_acl;
pub mod mdns;
pub mod privileges;
pub mod signals;

use std::sync::Arc;

use config::Config;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

/// Top-level daemon state.
pub struct Daemon {
    pub config: Config,
    shutdown: CancellationToken,
}

impl Daemon {
    /// Create a new daemon instance with the given configuration.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            shutdown: CancellationToken::new(),
        }
    }

    /// Initialize structured logging based on configuration.
    pub fn init_logging(&self) {
        let filter = EnvFilter::try_new(&self.config.daemon.log_level)
            .unwrap_or_else(|_| EnvFilter::new("info"));

        match self.config.daemon.log_format.as_str() {
            "json" => {
                fmt()
                    .json()
                    .with_env_filter(filter)
                    .with_target(true)
                    .with_thread_ids(true)
                    .init();
            }
            _ => {
                fmt().with_env_filter(filter).with_target(true).init();
            }
        }
    }

    /// Run the daemon. This is the main entry point after configuration is loaded.
    ///
    /// 1. Create PID file
    /// 2. Bind the API socket
    /// 3. Drop privileges (if configured)
    /// 4. Start signal handler
    /// 5. Run API server until shutdown
    /// 6. Clean up
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        // PID file — derive from socket path if not explicitly configured
        let pid_file = if self.config.daemon.pid_file.ends_with("/extender.pid") {
            // Use same directory as socket
            let socket_dir = std::path::Path::new(&self.config.daemon.socket_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/tmp".to_string());
            format!("{}/extender.pid", socket_dir)
        } else {
            self.config.daemon.pid_file.clone()
        };
        privileges::create_pid_file(&pid_file)?;

        // Drop privileges if configured.
        if let (Some(user), Some(group)) = (
            &self.config.daemon.drop_user,
            &self.config.daemon.drop_group,
        ) {
            privileges::drop_privileges(user, group)?;
        }

        // Start USB/IP TCP server (with optional TLS).
        let listen_addr = format!(
            "{}:{}",
            self.config.server.listen_address, self.config.server.port
        );
        let parsed_addr = listen_addr.parse()?;

        let tls_config = match (&self.config.server.tls_cert, &self.config.server.tls_key) {
            (Some(cert), Some(key)) => {
                info!(cert = %cert, key = %key, "TLS enabled for USB/IP server");
                Some(extender_server::TlsServerConfig {
                    cert_path: cert.clone(),
                    key_path: key.clone(),
                    ca_path: self.config.tls.ca.clone(),
                })
            }
            (Some(_), None) | (None, Some(_)) => {
                warn!("TLS partially configured: both tls_cert and tls_key are required. Starting without TLS.");
                None
            }
            _ => {
                info!("TLS not configured, USB/IP server using plain TCP");
                None
            }
        };

        let session_timeout_secs = self.config.server.session_timeout_secs;
        let server_engine = if let Some(ref tls_cfg) = tls_config {
            match extender_server::ServerEngine::new_tls_with_session_timeout(
                parsed_addr,
                tls_cfg,
                session_timeout_secs,
            )
            .await
            {
                Ok(engine) => {
                    info!(
                        listen = %self.config.server.listen_address,
                        port = self.config.server.port,
                        tls = true,
                        session_timeout_secs,
                        "USB/IP server listening (TLS)"
                    );
                    Some(engine)
                }
                Err(e) => {
                    warn!("failed to start TLS USB/IP server: {}", e);
                    None
                }
            }
        } else {
            match extender_server::ServerEngine::new_with_session_timeout(
                parsed_addr,
                session_timeout_secs,
            )
            .await
            {
                Ok(engine) => {
                    info!(
                        listen = %self.config.server.listen_address,
                        port = self.config.server.port,
                        session_timeout_secs,
                        "USB/IP server listening"
                    );
                    Some(engine)
                }
                Err(e) => {
                    warn!("failed to start USB/IP server: {}", e);
                    None
                }
            }
        };

        // Share the export registry between the TCP server and API handlers.
        let registry = server_engine
            .as_ref()
            .map(|e| Arc::clone(e.registry()))
            .unwrap_or_else(|| {
                Arc::new(extender_server::ExportRegistry::with_session_timeout(
                    self.config.server.session_timeout_secs,
                ))
            });

        let state = Arc::new(api_server::ApiState::new(
            registry.clone(),
            self.config.security.clone(),
        ));

        // Start mDNS service advertisement if enabled.
        let mdns_advertiser = if self.config.daemon.mdns_enabled {
            match mdns::MdnsAdvertiser::new(self.config.server.port, Arc::clone(&registry)) {
                Ok(advertiser) => {
                    info!("mDNS service advertisement started");
                    Some(advertiser)
                }
                Err(e) => {
                    warn!("failed to start mDNS advertisement: {}", e);
                    None
                }
            }
        } else {
            info!("mDNS advertisement disabled");
            None
        };

        // Signal handler with config reload on SIGHUP.
        let reload_config = {
            let socket_path = self.config.daemon.socket_path.clone();
            move || {
                info!("reloading configuration");
                let new_config = Config::load();
                info!(
                    "new log level: {}, socket: {}",
                    new_config.daemon.log_level, socket_path
                );
            }
        };

        let _signal_handle = signals::spawn_signal_handler(self.shutdown.clone(), reload_config);

        // Run USB/IP server and API server concurrently.
        let shutdown = self.shutdown.clone();
        let server_handle = if let Some(engine) = server_engine {
            let shutdown_token = shutdown.clone();
            Some(tokio::spawn(async move {
                if let Err(e) = engine.run_until_shutdown(shutdown_token.cancelled()).await {
                    tracing::warn!("USB/IP server error: {}", e);
                }
            }))
        } else {
            None
        };

        // Run the API server (blocks until shutdown).
        let api_result =
            api_server::run_api_server(&self.config.daemon.socket_path, shutdown, state).await;

        // Wait for server to finish.
        if let Some(handle) = server_handle {
            let _ = handle.await;
        }

        // Shut down mDNS advertisement.
        if let Some(advertiser) = mdns_advertiser {
            advertiser.shutdown();
        }

        // Cleanup.
        privileges::remove_pid_file(&pid_file);

        if let Err(e) = api_result {
            warn!("API server exited with error: {}", e);
        }

        info!("daemon shut down cleanly");
        Ok(())
    }

    /// Request a graceful shutdown.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Returns a clone of the shutdown token for external use.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}
