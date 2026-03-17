//! mDNS/DNS-SD service advertisement for the Extender daemon.
//!
//! When the daemon starts with mDNS enabled, it registers a DNS-SD service of
//! type `_usbip._tcp.local.` so that clients on the LAN can discover it
//! without knowing the IP address in advance.
//!
//! TXT records include `version=<crate version>` and `devices=<count>`.

use std::sync::Arc;

use extender_server::export::ExportRegistry;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{debug, info, warn};

/// The DNS-SD service type advertised by Extender daemons.
pub const SERVICE_TYPE: &str = "_usbip._tcp.local.";

/// Returns the local hostname as a `String`, falling back to `"extender"`.
fn get_hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "extender".to_string())
}

/// Handles mDNS service registration and deregistration.
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl MdnsAdvertiser {
    /// Create and register a new mDNS service advertisement.
    ///
    /// The service is published immediately with TXT records for the crate
    /// version and the current exported device count (read from `registry`).
    pub fn new(
        port: u16,
        registry: Arc<ExportRegistry>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let daemon = ServiceDaemon::new()?;

        let hostname = get_hostname();
        let instance_name = format!("Extender on {}", hostname);

        // Read the current exported device count (blocking-safe: we just read
        // the inner map's length via try_read or a sync wrapper).
        let device_count = {
            let inner = registry.inner();
            inner.try_read().map(|g| g.len()).unwrap_or(0)
        };

        let version = env!("CARGO_PKG_VERSION");
        let count_str = device_count.to_string();
        let properties: Vec<(&str, &str)> = vec![("version", version), ("devices", &count_str)];

        let host = format!("{}.local.", hostname);
        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
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

        Ok(Self { daemon, fullname })
    }

    /// Update the device count TXT record. Call this when devices are
    /// bound or unbound.
    pub fn update_device_count(&self, count: u32) {
        debug!(count, "updating mDNS device count TXT record");

        let hostname = get_hostname();
        let instance_name = format!("Extender on {}", hostname);
        let host = format!("{}.local.", hostname);
        let version = env!("CARGO_PKG_VERSION");
        let count_str = count.to_string();
        let properties: Vec<(&str, &str)> = vec![("version", version), ("devices", &count_str)];

        match ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &host,
            "",
            0,
            properties.as_slice(),
        ) {
            Ok(info) => {
                if let Err(e) = self.daemon.register(info) {
                    warn!("failed to update mDNS TXT record: {}", e);
                }
            }
            Err(e) => {
                warn!("failed to build mDNS service info for update: {}", e);
            }
        }
    }

    /// Return the fullname of the registered service.
    pub fn fullname(&self) -> &str {
        &self.fullname
    }

    /// Deregister the service and shut down the mDNS daemon.
    pub fn shutdown(self) {
        debug!(fullname = %self.fullname, "deregistering mDNS service");
        if let Err(e) = self.daemon.unregister(&self.fullname) {
            warn!("failed to deregister mDNS service: {}", e);
        }
        if let Err(e) = self.daemon.shutdown() {
            warn!("failed to shut down mDNS daemon: {}", e);
        }
        info!("mDNS service deregistered");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_mdns_register_and_shutdown() {
        // Verify that registration and clean shutdown work without errors.
        let registry = Arc::new(ExportRegistry::new());
        let advertiser = MdnsAdvertiser::new(13240, registry).unwrap();
        assert!(!advertiser.fullname().is_empty());
        advertiser.shutdown();
    }

    #[test]
    fn test_mdns_update_device_count() {
        let registry = Arc::new(ExportRegistry::new());
        let advertiser = MdnsAdvertiser::new(13241, registry).unwrap();
        // Should not panic or error.
        advertiser.update_device_count(5);
        advertiser.shutdown();
    }

    #[test]
    fn test_mdns_register_and_discover() {
        let registry = Arc::new(ExportRegistry::new());
        let advertiser = MdnsAdvertiser::new(13242, registry).unwrap();

        // Browse for the service using a separate daemon.
        let browser = ServiceDaemon::new().unwrap();
        let receiver = browser.browse(SERVICE_TYPE).unwrap();

        let mut found = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                    if info.get_fullname() == advertiser.fullname() {
                        assert_eq!(
                            info.get_property_val_str("version"),
                            Some(env!("CARGO_PKG_VERSION"))
                        );
                        assert_eq!(info.get_property_val_str("devices"), Some("0"));
                        found = true;
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }

        let _ = browser.stop_browse(SERVICE_TYPE);
        let _ = browser.shutdown();
        advertiser.shutdown();

        // Discovery depends on multicast networking being available, which
        // may not be the case in all CI/test environments.
        if !found {
            eprintln!(
                "warning: mDNS discovery did not find the service (multicast may be unavailable)"
            );
        }
    }
}
