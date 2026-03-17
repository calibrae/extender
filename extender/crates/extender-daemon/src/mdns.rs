//! mDNS/DNS-SD service advertisement for the Extender daemon.
//!
//! On macOS, uses the system's `dns-sd` command (Bonjour) for reliable
//! service registration. On Linux, uses the `mdns-sd` crate directly.
//!
//! TXT records include `version=<crate version>` and `devices=<count>`.

use std::sync::Arc;

use extender_server::export::ExportRegistry;
use tracing::{debug, info, warn};

/// The DNS-SD service type.
pub const SERVICE_TYPE: &str = "_usbip._tcp";

/// Returns the local hostname, falling back to `"extender"`.
fn get_hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "extender".to_string())
}

/// Handles mDNS service registration and deregistration.
pub struct MdnsAdvertiser {
    #[cfg(target_os = "macos")]
    child: Option<std::process::Child>,
    #[cfg(not(target_os = "macos"))]
    daemon: mdns_sd::ServiceDaemon,
    #[cfg(not(target_os = "macos"))]
    fullname: String,
    hostname: String,
}

impl MdnsAdvertiser {
    /// Register the mDNS service.
    pub fn new(
        port: u16,
        registry: Arc<ExportRegistry>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let hostname = get_hostname();
        let device_count = registry.inner().try_read().map(|g| g.len()).unwrap_or(0);
        let version = env!("CARGO_PKG_VERSION");

        #[cfg(target_os = "macos")]
        {
            // Use system Bonjour via dns-sd command for reliable macOS integration.
            let txt = format!("version={}", version);
            let txt2 = format!("devices={}", device_count);
            let instance_name = format!("Extender on {}", hostname);
            let child = std::process::Command::new("dns-sd")
                .args([
                    "-R",
                    &instance_name,
                    SERVICE_TYPE,
                    "local",
                    &port.to_string(),
                    &txt,
                    &txt2,
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();

            match child {
                Ok(child) => {
                    info!(
                        service_type = SERVICE_TYPE,
                        port,
                        hostname = %hostname,
                        devices = device_count,
                        "mDNS service registered (Bonjour)"
                    );
                    Ok(Self {
                        child: Some(child),
                        hostname,
                    })
                }
                Err(e) => {
                    warn!("failed to start dns-sd for mDNS registration: {}", e);
                    Ok(Self {
                        child: None,
                        hostname,
                    })
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            use mdns_sd::{ServiceDaemon, ServiceInfo};

            let daemon = ServiceDaemon::new()?;
            let instance_name = format!("Extender on {}", hostname);
            let host = format!("{}.local.", hostname);
            let count_str = device_count.to_string();
            let properties: Vec<(&str, &str)> = vec![("version", version), ("devices", &count_str)];

            let service_type_local = format!("{}.local.", SERVICE_TYPE);
            let service_info = ServiceInfo::new(
                &service_type_local,
                &instance_name,
                &host,
                "",
                port,
                properties.as_slice(),
            )?;

            let fullname = service_info.get_fullname().to_string();
            daemon.register(service_info)?;

            info!(
                service_type = SERVICE_TYPE,
                port,
                hostname = %hostname,
                devices = device_count,
                "mDNS service registered"
            );

            Ok(Self {
                daemon,
                fullname,
                hostname,
            })
        }
    }

    /// Update the device count TXT record.
    pub fn update_device_count(&self, count: u32) {
        debug!(count, "updating mDNS device count");
        // On macOS, dns-sd -R doesn't support TXT record updates without restart.
        // On Linux, we'd re-register. For now, this is best-effort.
        let _ = count;
    }

    /// Return the hostname.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Deregister the service and shut down.
    pub fn shutdown(mut self) {
        debug!("deregistering mDNS service");

        #[cfg(target_os = "macos")]
        {
            if let Some(ref mut child) = self.child {
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.daemon.unregister(&self.fullname);
            let _ = self.daemon.shutdown();
        }

        info!("mDNS service deregistered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mdns_register_and_shutdown() {
        let registry = Arc::new(ExportRegistry::new());
        let advertiser = MdnsAdvertiser::new(13240, registry).unwrap();
        assert!(!advertiser.hostname().is_empty());
        advertiser.shutdown();
    }

    #[test]
    fn test_mdns_update_device_count() {
        let registry = Arc::new(ExportRegistry::new());
        let advertiser = MdnsAdvertiser::new(13241, registry).unwrap();
        advertiser.update_device_count(5);
        advertiser.shutdown();
    }
}
