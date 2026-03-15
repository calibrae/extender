//! Extender daemon: orchestrates server/client engines and serves the API.
//!
//! The daemon binds a Unix domain socket, listens for JSON-RPC requests from
//! the CLI (or other clients), and delegates to the server and client engines.

pub mod api_server;
pub mod config;
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
        // PID file
        privileges::create_pid_file(&self.config.daemon.pid_file)?;

        // Drop privileges if configured.
        if let (Some(user), Some(group)) = (
            &self.config.daemon.drop_user,
            &self.config.daemon.drop_group,
        ) {
            privileges::drop_privileges(user, group)?;
        }

        // Shared API state.
        let state = Arc::new(api_server::ApiState::new());

        // Signal handler with config reload on SIGHUP.
        let reload_config = {
            let socket_path = self.config.daemon.socket_path.clone();
            move || {
                info!("reloading configuration");
                let new_config = Config::load();
                // Hot-reload: update log level filter.
                // In a production implementation, we would update a shared Arc<RwLock<Config>>.
                // For now we just log what would change.
                info!(
                    "new log level: {}, socket: {}",
                    new_config.daemon.log_level, socket_path
                );
            }
        };

        let _signal_handle = signals::spawn_signal_handler(self.shutdown.clone(), reload_config);

        // Run the API server.
        let api_result = api_server::run_api_server(
            &self.config.daemon.socket_path,
            self.shutdown.clone(),
            state,
        )
        .await;

        // Cleanup.
        privileges::remove_pid_file(&self.config.daemon.pid_file);

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
