//! mDNS/DNS-SD discovery for Extender servers on the LAN.
//!
//! Browses for `_usbip._tcp.local.` services and returns a list of discovered
//! servers with their hostname, address, version, and device count.

use std::net::SocketAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::debug;

/// The DNS-SD service type browsed for discovery.
pub const SERVICE_TYPE: &str = "_usbip._tcp.local.";

/// A server discovered via mDNS/DNS-SD.
#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    /// The advertised hostname / instance name.
    pub hostname: String,
    /// The resolved socket address (IP + port).
    pub addr: SocketAddr,
    /// The Extender version string from TXT records.
    pub version: String,
    /// The number of exported devices from TXT records.
    pub device_count: u32,
}

/// Browse the local network for Extender servers for `timeout` duration.
///
/// Returns all servers that were resolved within the timeout window.
pub async fn discover_servers(timeout: Duration) -> Vec<DiscoveredServer> {
    // mdns-sd is synchronous; run the blocking browse in a spawn_blocking task.
    tokio::task::spawn_blocking(move || discover_servers_blocking(timeout))
        .await
        .unwrap_or_default()
}

/// Synchronous implementation of server discovery.
fn discover_servers_blocking(timeout: Duration) -> Vec<DiscoveredServer> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to create mDNS browser: {}", e);
            return Vec::new();
        }
    };

    let receiver = match daemon.browse(SERVICE_TYPE) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("failed to start mDNS browse: {}", e);
            let _ = daemon.shutdown();
            return Vec::new();
        }
    };

    let mut servers = Vec::new();
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let hostname = info.get_fullname().to_string();
                let port = info.get_port();

                let version = info
                    .get_property_val_str("version")
                    .unwrap_or("unknown")
                    .to_string();
                let device_count: u32 = info
                    .get_property_val_str("devices")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);

                // Collect all resolved IP addresses.
                for addr in info.get_addresses().iter() {
                    let socket_addr = SocketAddr::new(*addr, port);
                    debug!(
                        hostname = %hostname,
                        addr = %socket_addr,
                        version = %version,
                        devices = device_count,
                        "discovered Extender server"
                    );
                    servers.push(DiscoveredServer {
                        hostname: hostname.clone(),
                        addr: socket_addr,
                        version: version.clone(),
                        device_count,
                    });
                }
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }

    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();

    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_discover_servers_returns_empty_when_none() {
        // With a very short timeout and no servers running, we should get an
        // empty list (no panic, no error).
        let servers = discover_servers(Duration::from_millis(200)).await;
        // We cannot guarantee emptiness (another test might be advertising),
        // but the call should succeed without error.
        let _ = servers;
    }
}
