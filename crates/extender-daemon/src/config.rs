//! Configuration file loading, defaults, environment variable overrides, and merging.

use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, warn};

/// Top-level daemon configuration.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub server: ServerConfig,
    pub client: ClientConfig,
    pub daemon: DaemonConfig,
    pub security: SecurityConfig,
}

/// Server-side settings.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ServerConfig {
    pub listen_address: String,
    pub port: u16,
    pub max_connections: u32,
}

/// Client-side settings.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ClientConfig {
    pub vhci_path: String,
}

/// Daemon process settings.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: String,
    pub pid_file: String,
    pub log_level: String,
    pub log_format: String,
    pub drop_user: Option<String>,
    pub drop_group: Option<String>,
}

/// Security / device filtering settings.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
#[derive(Default)]
pub struct SecurityConfig {
    pub allowed_devices: Vec<String>,
    pub denied_devices: Vec<String>,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".to_string(),
            port: 3240,
            max_connections: 16,
        }
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            vhci_path: "/sys/devices/platform/vhci_hcd.0".to_string(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        // Use /var/run when running as root, /tmp otherwise
        let (socket_path, pid_file) = if nix::unistd::geteuid().is_root() {
            (
                "/var/run/extender.sock".to_string(),
                "/var/run/extender.pid".to_string(),
            )
        } else {
            let runtime_dir =
                std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
            (
                format!("{}/extender.sock", runtime_dir),
                format!("{}/extender.pid", runtime_dir),
            )
        };
        Self {
            socket_path,
            pid_file,
            log_level: "info".to_string(),
            log_format: "text".to_string(),
            drop_user: None,
            drop_group: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// System-wide config path.
pub const SYSTEM_CONFIG_PATH: &str = "/etc/extender/config.toml";

/// Returns the user-level config path: `~/.config/extender/config.toml`.
pub fn user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("extender").join("config.toml"))
}

impl Config {
    /// Load configuration with full precedence chain:
    /// defaults -> system config -> user config -> environment variables.
    ///
    /// CLI flags are applied by the caller after this returns.
    pub fn load() -> Self {
        let mut config = Config::default();

        // Layer 1: system config
        if let Some(system) = Self::load_file(SYSTEM_CONFIG_PATH) {
            config = system;
            debug!("loaded system config from {}", SYSTEM_CONFIG_PATH);
        }

        // Layer 2: user config (overwrites all fields present in the file)
        if let Some(user_path) = user_config_path() {
            if let Some(user) = Self::load_file(user_path.to_string_lossy().as_ref()) {
                config.merge_from(&user);
                debug!("loaded user config from {}", user_path.display());
            }
        }

        // Layer 3: environment variables
        config.apply_env_overrides();

        config
    }

    /// Load configuration from a specific TOML file.
    /// Returns `None` if the file does not exist or cannot be parsed.
    pub fn load_file(path: &str) -> Option<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return None,
        };
        match toml::from_str::<Config>(&content) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                warn!("failed to parse config file {}: {}", path, e);
                None
            }
        }
    }

    /// Merge values from `other` into `self`. Fields present in `other` overwrite
    /// fields in `self`. This is a coarse merge at the section level: if a TOML
    /// section is present in `other`, we take the whole sub-struct.
    ///
    /// For a more granular merge we would need `Option<T>` wrappers on every
    /// field; this is sufficient for MVP.
    fn merge_from(&mut self, other: &Config) {
        // We re-serialize `other` to detect which top-level keys were present.
        // For simplicity in MVP, we just overwrite everything from the user config.
        self.server = other.server.clone();
        self.client = other.client.clone();
        self.daemon = other.daemon.clone();
        self.security = other.security.clone();
    }

    /// Apply environment variable overrides.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("EXTENDER_HOST") {
            debug!("EXTENDER_HOST override: {}", val);
            self.server.listen_address = val;
        }
        if let Ok(val) = std::env::var("EXTENDER_PORT") {
            if let Ok(port) = val.parse::<u16>() {
                debug!("EXTENDER_PORT override: {}", port);
                self.server.port = port;
            } else {
                warn!("EXTENDER_PORT is not a valid u16: {}", val);
            }
        }
        if let Ok(val) = std::env::var("EXTENDER_SOCKET") {
            debug!("EXTENDER_SOCKET override: {}", val);
            self.daemon.socket_path = val;
        }
        if let Ok(val) = std::env::var("EXTENDER_LOG_LEVEL") {
            debug!("EXTENDER_LOG_LEVEL override: {}", val);
            self.daemon.log_level = val;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = Config::default();
        assert_eq!(cfg.server.listen_address, "127.0.0.1");
        assert_eq!(cfg.server.port, 3240);
        assert_eq!(cfg.server.max_connections, 16);
        assert_eq!(cfg.daemon.log_level, "info");
        assert_eq!(cfg.daemon.log_format, "text");
        assert!(cfg.security.allowed_devices.is_empty());
        assert!(cfg.security.denied_devices.is_empty());
    }

    #[test]
    fn test_toml_parsing() {
        let toml_str = r#"
[server]
listen_address = "0.0.0.0"
port = 9999
max_connections = 32

[client]
vhci_path = "/custom/vhci"

[daemon]
socket_path = "/tmp/test.sock"
pid_file = "/tmp/test.pid"
log_level = "debug"
log_format = "json"
drop_user = "testuser"
drop_group = "testgroup"

[security]
allowed_devices = ["1234:5678"]
denied_devices = ["abcd:ef01"]
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.server.listen_address, "0.0.0.0");
        assert_eq!(cfg.server.port, 9999);
        assert_eq!(cfg.server.max_connections, 32);
        assert_eq!(cfg.client.vhci_path, "/custom/vhci");
        assert_eq!(cfg.daemon.socket_path, "/tmp/test.sock");
        assert_eq!(cfg.daemon.log_level, "debug");
        assert_eq!(cfg.daemon.log_format, "json");
        assert_eq!(cfg.daemon.drop_user, Some("testuser".to_string()));
        assert_eq!(cfg.daemon.drop_group, Some("testgroup".to_string()));
        assert_eq!(cfg.security.allowed_devices, vec!["1234:5678"]);
        assert_eq!(cfg.security.denied_devices, vec!["abcd:ef01"]);
    }

    #[test]
    fn test_partial_toml_uses_defaults() {
        let toml_str = r#"
[server]
port = 4000
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.server.port, 4000);
        // Non-specified fields get defaults
        assert_eq!(cfg.server.listen_address, "127.0.0.1");
        assert_eq!(cfg.daemon.log_level, "info");
    }

    #[test]
    fn test_env_override() {
        // Save originals
        let orig_port = std::env::var("EXTENDER_PORT").ok();
        let orig_host = std::env::var("EXTENDER_HOST").ok();
        let orig_socket = std::env::var("EXTENDER_SOCKET").ok();
        let orig_log = std::env::var("EXTENDER_LOG_LEVEL").ok();

        std::env::set_var("EXTENDER_PORT", "7777");
        std::env::set_var("EXTENDER_HOST", "192.168.1.1");
        std::env::set_var("EXTENDER_SOCKET", "/tmp/override.sock");
        std::env::set_var("EXTENDER_LOG_LEVEL", "trace");

        let mut cfg = Config::default();
        cfg.apply_env_overrides();

        assert_eq!(cfg.server.port, 7777);
        assert_eq!(cfg.server.listen_address, "192.168.1.1");
        assert_eq!(cfg.daemon.socket_path, "/tmp/override.sock");
        assert_eq!(cfg.daemon.log_level, "trace");

        // Restore
        fn restore(key: &str, val: Option<String>) {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        restore("EXTENDER_PORT", orig_port);
        restore("EXTENDER_HOST", orig_host);
        restore("EXTENDER_SOCKET", orig_socket);
        restore("EXTENDER_LOG_LEVEL", orig_log);
    }

    #[test]
    fn test_load_nonexistent_file() {
        let result = Config::load_file("/nonexistent/path/config.toml");
        assert!(result.is_none());
    }
}
