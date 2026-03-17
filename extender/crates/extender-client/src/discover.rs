//! mDNS/DNS-SD discovery for Extender servers on the LAN.
//!
//! On macOS, uses the system's `dns-sd -B` command for reliable Bonjour
//! integration. On Linux, uses the `mdns-sd` crate directly.

use std::net::SocketAddr;
use std::time::Duration;

use tracing::debug;

/// The DNS-SD service type browsed for discovery.
pub const SERVICE_TYPE: &str = "_usbip._tcp";

/// A server discovered via mDNS/DNS-SD.
#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    /// The advertised instance name.
    pub hostname: String,
    /// The resolved socket address (IP + port).
    pub addr: SocketAddr,
    /// The Extender version string from TXT records.
    pub version: String,
    /// The number of exported devices from TXT records.
    pub device_count: u32,
}

/// Browse the local network for Extender servers for `timeout` duration.
pub async fn discover_servers(timeout: Duration) -> Vec<DiscoveredServer> {
    tokio::task::spawn_blocking(move || discover_servers_blocking(timeout))
        .await
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn discover_servers_blocking(timeout: Duration) -> Vec<DiscoveredServer> {
    // Use dns-sd -Z to get full service info (browse + resolve in one shot).
    // dns-sd -B _usbip._tcp shows instances but not IPs.
    // dns-sd -Z _usbip._tcp shows full records but needs parsing.
    // Simpler: use dns-sd -B to find instances, then dns-sd -L to resolve each.

    let output = match std::process::Command::new("dns-sd")
        .args(["-Z", SERVICE_TYPE, "local"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::sleep(timeout);
            let _ = child.kill();
            child.wait_with_output().ok()
        }
        Err(e) => {
            tracing::warn!("failed to run dns-sd: {}", e);
            return Vec::new();
        }
    };

    let output = match output {
        Some(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        None => return Vec::new(),
    };

    parse_dns_sd_output(&output)
}

#[cfg(target_os = "macos")]
fn parse_dns_sd_output(output: &str) -> Vec<DiscoveredServer> {
    let mut servers = Vec::new();

    // dns-sd -Z output format:
    // _usbip._tcp                                     PTR     Extender on calimba._usbip._tcp.local.
    // Extender on calimba._usbip._tcp.local.  SRV     0 0 3240 calimba.local.
    // Extender on calimba._usbip._tcp.local.  TXT     "version=0.1.0" "devices=1"

    // dns-sd escapes spaces as \032 and dots as \. — unescape them.
    let unescape = |s: &str| {
        s.replace("\\032", " ")
            .replace("\\.", ".")
            .replace("\\", "")
    };

    let mut current_name = String::new();
    let mut current_port: u16 = 0;
    let mut current_host = String::new();
    let mut current_version = String::new();
    let mut current_devices: u32 = 0;

    for line in output.lines() {
        let line = line.trim();

        if line.contains("SRV") {
            // Parse: <name> SRV <priority> <weight> <port> <host>
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 6 {
                current_name = unescape(parts[0].split("._usbip").next().unwrap_or(parts[0]));
                current_port = parts[4].parse().unwrap_or(3240);
                current_host = parts[5].trim_end_matches('.').to_string();
            }
        } else if line.contains("TXT") {
            // Parse: <name> TXT "key=value" "key=value"
            for part in line.split('"') {
                if let Some(val) = part.strip_prefix("version=") {
                    current_version = val.to_string();
                } else if let Some(val) = part.strip_prefix("devices=") {
                    current_devices = val.parse().unwrap_or(0);
                }
            }

            // After TXT, we have a complete record — try to resolve the host
            if !current_host.is_empty() && current_port > 0 {
                if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&format!(
                    "{}:{}",
                    current_host, current_port
                )) {
                    // Prefer IPv4 over IPv6 for display
                    let all: Vec<_> = addrs.collect();
                    let addr = all.iter().find(|a| a.is_ipv4()).or(all.first()).copied();

                    if let Some(addr) = addr {
                        // Skip if we already have this server
                        if !servers
                            .iter()
                            .any(|s: &DiscoveredServer| s.hostname == current_name)
                        {
                            debug!(
                                hostname = %current_name,
                                addr = %addr,
                                version = %current_version,
                                devices = current_devices,
                                "discovered Extender server (Bonjour)"
                            );
                            servers.push(DiscoveredServer {
                                hostname: current_name.clone(),
                                addr,
                                version: current_version.clone(),
                                device_count: current_devices,
                            });
                        }
                    }
                }

                // Reset for next record
                current_name.clear();
                current_host.clear();
                current_port = 0;
                current_version.clear();
                current_devices = 0;
            }
        }
    }

    servers
}

#[cfg(not(target_os = "macos"))]
fn discover_servers_blocking(timeout: Duration) -> Vec<DiscoveredServer> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};

    let service_type_local = format!("{}.local.", SERVICE_TYPE);

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to create mDNS browser: {}", e);
            return Vec::new();
        }
    };

    let receiver = match daemon.browse(&service_type_local) {
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

    let _ = daemon.stop_browse(&service_type_local);
    let _ = daemon.shutdown();

    // Deduplicate by address
    servers.sort_by_key(|s| s.addr.to_string());
    servers.dedup_by(|a, b| a.addr == b.addr);
    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_discover_servers_returns_without_panic() {
        let servers = discover_servers(Duration::from_millis(500)).await;
        let _ = servers; // Just verify no panic
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_parse_dns_sd_output() {
        let output = r#"
_usbip._tcp                                     PTR     Extender\032on\032calimba._usbip._tcp.local.
Extender\032on\032calimba._usbip._tcp.local.    SRV     0 0 3240 calimba.local.
Extender\032on\032calimba._usbip._tcp.local.    TXT     "version=0.1.0" "devices=1"
"#;
        let servers = parse_dns_sd_output(output);
        // May or may not resolve calimba.local depending on environment
        // but parsing should not panic
        let _ = servers;
    }
}
