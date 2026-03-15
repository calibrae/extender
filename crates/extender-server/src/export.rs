//! Device export registry for tracking bound (exported) USB devices.
//!
//! The [`ExportRegistry`] maintains a thread-safe map of bus IDs to
//! [`ExportedDevice`] entries. The server engine uses this registry to
//! determine which devices are available for remote import.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::device::LocalUsbDevice;
use crate::error::ServerError;
use crate::handle::ManagedDevice;

/// Unique identifier for an active import session.
pub type SessionId = u64;

/// A USB device that has been exported (bound) for remote access.
#[derive(Debug)]
pub struct ExportedDevice {
    /// The local USB device metadata.
    pub device: LocalUsbDevice,
    /// The opened and claimed device handle.
    pub handle: Arc<ManagedDevice>,
    /// If a client currently has this device imported, this holds the session ID.
    pub active_session: Option<SessionId>,
}

/// Thread-safe registry of exported USB devices.
///
/// Devices must be explicitly bound before they can be listed or imported
/// by remote clients.
#[derive(Debug, Clone)]
pub struct ExportRegistry {
    devices: Arc<RwLock<HashMap<String, ExportedDevice>>>,
    next_session_id: Arc<std::sync::atomic::AtomicU64>,
}

impl ExportRegistry {
    /// Create a new, empty export registry.
    pub fn new() -> Self {
        ExportRegistry {
            devices: Arc::new(RwLock::new(HashMap::new())),
            next_session_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    /// Bind (export) a device by bus ID, making it available for remote import.
    ///
    /// This enumerates local USB devices, finds the one matching `bus_id`,
    /// opens a managed handle (claiming all interfaces), and adds it to the
    /// registry.
    ///
    /// Returns an error if:
    /// - The device is not found locally
    /// - The device is already bound
    /// - The device handle cannot be opened
    pub async fn bind_device(&self, bus_id: &str) -> Result<(), ServerError> {
        // Check if already bound.
        {
            let devices = self.devices.read().await;
            if devices.contains_key(bus_id) {
                return Err(ServerError::DeviceAlreadyBound {
                    bus_id: bus_id.to_string(),
                });
            }
        }

        // Enumerate local devices and open a handle.
        let local_devices = crate::device::enumerate_devices()?;
        let local = local_devices
            .iter()
            .find(|d| d.bus_id == bus_id)
            .ok_or_else(|| ServerError::DeviceNotFound {
                bus_id: bus_id.to_string(),
            })?
            .clone();

        let handle = ManagedDevice::open(&local_devices, bus_id)?;

        let exported = ExportedDevice {
            device: local,
            handle,
            active_session: None,
        };

        let mut devices = self.devices.write().await;
        // Double-check after acquiring write lock.
        if devices.contains_key(bus_id) {
            return Err(ServerError::DeviceAlreadyBound {
                bus_id: bus_id.to_string(),
            });
        }
        devices.insert(bus_id.to_string(), exported);

        tracing::info!(bus_id, "device bound for export");
        Ok(())
    }

    /// Bind a device using pre-constructed components (for testing or
    /// external handle management).
    pub async fn bind_device_with(
        &self,
        bus_id: &str,
        device: LocalUsbDevice,
        handle: Arc<ManagedDevice>,
    ) -> Result<(), ServerError> {
        let mut devices = self.devices.write().await;
        if devices.contains_key(bus_id) {
            return Err(ServerError::DeviceAlreadyBound {
                bus_id: bus_id.to_string(),
            });
        }
        devices.insert(
            bus_id.to_string(),
            ExportedDevice {
                device,
                handle,
                active_session: None,
            },
        );
        Ok(())
    }

    /// Unbind (unexport) a device by bus ID.
    ///
    /// Removes the device from the registry and drops the handle, which
    /// releases all claimed interfaces. If the device has an active session,
    /// the session ID is returned so the caller can clean it up.
    pub async fn unbind_device(&self, bus_id: &str) -> Result<Option<SessionId>, ServerError> {
        let mut devices = self.devices.write().await;
        let exported = devices
            .remove(bus_id)
            .ok_or_else(|| ServerError::DeviceNotBound {
                bus_id: bus_id.to_string(),
            })?;

        tracing::info!(bus_id, "device unbound from export");
        Ok(exported.active_session)
    }

    /// Get the list of all exported devices as protocol-level device descriptors.
    ///
    /// Used to build the OP_REP_DEVLIST response.
    pub async fn list_devices(&self) -> Result<Vec<extender_protocol::UsbDevice>, ServerError> {
        let devices = self.devices.read().await;
        let mut result = Vec::with_capacity(devices.len());
        for exported in devices.values() {
            result.push(exported.device.to_protocol_device()?);
        }
        Ok(result)
    }

    /// Try to acquire a device for an import session.
    ///
    /// If the device is bound and not in use, marks it as in-use and returns
    /// the device handle and protocol device descriptor. Otherwise returns an error.
    pub async fn try_acquire(
        &self,
        bus_id: &str,
    ) -> Result<(Arc<ManagedDevice>, extender_protocol::UsbDevice, SessionId), ServerError> {
        let mut devices = self.devices.write().await;
        let exported = devices
            .get_mut(bus_id)
            .ok_or_else(|| ServerError::DeviceNotBound {
                bus_id: bus_id.to_string(),
            })?;

        if exported.active_session.is_some() {
            return Err(ServerError::DeviceInUse {
                bus_id: bus_id.to_string(),
            });
        }

        let session_id = self
            .next_session_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        exported.active_session = Some(session_id);

        let handle = Arc::clone(&exported.handle);
        let proto_device = exported.device.to_protocol_device()?;

        tracing::info!(bus_id, session_id, "device acquired for import session");
        Ok((handle, proto_device, session_id))
    }

    /// Release a device from an import session.
    ///
    /// Clears the active session marker so the device can be imported again.
    pub async fn release(&self, bus_id: &str, session_id: SessionId) {
        let mut devices = self.devices.write().await;
        if let Some(exported) = devices.get_mut(bus_id) {
            if exported.active_session == Some(session_id) {
                exported.active_session = None;
                tracing::info!(bus_id, session_id, "device released from import session");
            }
        }
    }

    /// Get a reference to the inner devices map (for testing).
    pub fn inner(&self) -> &Arc<RwLock<HashMap<String, ExportedDevice>>> {
        &self.devices
    }
}

impl Default for ExportRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_new_is_empty() {
        let registry = ExportRegistry::new();
        let list = registry.list_devices().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_unbind_not_bound_error() {
        let registry = ExportRegistry::new();
        let result = registry.unbind_device("1-1").await;
        assert!(matches!(result, Err(ServerError::DeviceNotBound { .. })));
    }

    #[tokio::test]
    async fn test_acquire_not_bound_error() {
        let registry = ExportRegistry::new();
        let result = registry.try_acquire("1-1").await;
        assert!(matches!(result, Err(ServerError::DeviceNotBound { .. })));
    }

    #[tokio::test]
    async fn test_release_nonexistent_is_noop() {
        let registry = ExportRegistry::new();
        // Should not panic.
        registry.release("1-1", 42).await;
    }

    // Integration-level test using bind_device (requires real USB).
    #[tokio::test]
    #[ignore]
    async fn test_bind_device_not_found() {
        let registry = ExportRegistry::new();
        let result = registry.bind_device("99-99").await;
        assert!(matches!(result, Err(ServerError::DeviceNotFound { .. })));
    }
}
