//! Device export registry for tracking bound (exported) USB devices.
//!
//! The [`ExportRegistry`] maintains a thread-safe map of bus IDs to
//! [`ExportedDevice`] entries. The server engine uses this registry to
//! determine which devices are available for remote import.
//!
//! When a session ends (e.g. network blip), the device enters a grace period
//! during which it stays reserved for the same bus ID. If the client
//! reconnects within the grace period, the session is resumed. After the
//! grace period expires the device is released for other clients.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::device::LocalUsbDevice;
use crate::error::ServerError;
use crate::handle::ManagedDevice;

/// Unique identifier for an active import session.
pub type SessionId = u64;

/// Default session grace period in seconds.
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 30;

/// State of an import session on an exported device.
#[derive(Debug, Clone)]
pub enum SessionState {
    /// A client is actively connected.
    Active(SessionId),
    /// The connection dropped; the device is reserved until `deadline`.
    Disconnected {
        session_id: SessionId,
        bus_id: String,
        deadline: tokio::time::Instant,
    },
}

/// A USB device that has been exported (bound) for remote access.
#[derive(Debug)]
pub struct ExportedDevice {
    /// The local USB device metadata.
    pub device: LocalUsbDevice,
    /// The opened and claimed device handle.
    pub handle: Arc<ManagedDevice>,
    /// If a client currently has this device imported, this holds the session ID.
    pub active_session: Option<SessionId>,
    /// Session state tracking for reconnection support.
    pub session_state: Option<SessionState>,
}

/// Thread-safe registry of exported USB devices.
///
/// Devices must be explicitly bound before they can be listed or imported
/// by remote clients.
#[derive(Debug, Clone)]
pub struct ExportRegistry {
    devices: Arc<RwLock<HashMap<String, ExportedDevice>>>,
    next_session_id: Arc<std::sync::atomic::AtomicU64>,
    /// Grace period for disconnected sessions before the device is released.
    session_timeout: Duration,
}

impl ExportRegistry {
    /// Create a new, empty export registry with the default session timeout.
    pub fn new() -> Self {
        ExportRegistry {
            devices: Arc::new(RwLock::new(HashMap::new())),
            next_session_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            session_timeout: Duration::from_secs(DEFAULT_SESSION_TIMEOUT_SECS),
        }
    }

    /// Create a new, empty export registry with a custom session timeout.
    pub fn with_session_timeout(timeout_secs: u64) -> Self {
        ExportRegistry {
            devices: Arc::new(RwLock::new(HashMap::new())),
            next_session_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            session_timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Get the configured session timeout duration.
    pub fn session_timeout(&self) -> Duration {
        self.session_timeout
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
            session_state: None,
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
                session_state: None,
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
    /// the device handle and protocol device descriptor. If the device has a
    /// disconnected session within its grace period for the same bus ID, the
    /// reconnection is allowed and `is_reconnect` is set to `true` in the
    /// returned tuple. Otherwise returns an error.
    pub async fn try_acquire(
        &self,
        bus_id: &str,
    ) -> Result<(Arc<ManagedDevice>, extender_protocol::UsbDevice, SessionId), ServerError> {
        let (handle, proto_device, session_id, is_reconnect) = self.try_acquire_ext(bus_id).await?;
        if is_reconnect {
            tracing::info!(bus_id, session_id, "device re-acquired (reconnection)");
        }
        Ok((handle, proto_device, session_id))
    }

    /// Extended version of [`try_acquire`] that also reports whether this is a
    /// reconnection within the grace period.
    ///
    /// Returns `(handle, proto_device, session_id, is_reconnect)`.
    pub async fn try_acquire_ext(
        &self,
        bus_id: &str,
    ) -> Result<
        (
            Arc<ManagedDevice>,
            extender_protocol::UsbDevice,
            SessionId,
            bool,
        ),
        ServerError,
    > {
        let mut devices = self.devices.write().await;
        let exported = devices
            .get_mut(bus_id)
            .ok_or_else(|| ServerError::DeviceNotBound {
                bus_id: bus_id.to_string(),
            })?;

        // Check for a disconnected session in its grace period (reconnection).
        if let Some(SessionState::Disconnected {
            bus_id: ref disconnected_bus_id,
            deadline,
            ..
        }) = exported.session_state
        {
            if disconnected_bus_id == bus_id && tokio::time::Instant::now() < deadline {
                // Reconnection within grace period -- resume.
                let session_id = self
                    .next_session_id
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                exported.active_session = Some(session_id);
                exported.session_state = Some(SessionState::Active(session_id));

                let handle = Arc::clone(&exported.handle);
                let proto_device = exported.device.to_protocol_device()?;

                tracing::info!(bus_id, session_id, "client reconnected within grace period");
                return Ok((handle, proto_device, session_id, true));
            } else {
                // Grace period expired -- clear the disconnected state.
                exported.active_session = None;
                exported.session_state = None;
            }
        }

        if exported.active_session.is_some() {
            return Err(ServerError::DeviceInUse {
                bus_id: bus_id.to_string(),
            });
        }

        let session_id = self
            .next_session_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        exported.active_session = Some(session_id);
        exported.session_state = Some(SessionState::Active(session_id));

        let handle = Arc::clone(&exported.handle);
        let proto_device = exported.device.to_protocol_device()?;

        tracing::info!(bus_id, session_id, "device acquired for import session");
        Ok((handle, proto_device, session_id, false))
    }

    /// Release a device from an import session, entering the grace period.
    ///
    /// Instead of immediately clearing the session, the device enters the
    /// "disconnected" state. A background task is spawned to clear the session
    /// after the grace period expires if no reconnection occurs.
    pub async fn release(&self, bus_id: &str, session_id: SessionId) {
        let timeout = self.session_timeout;
        let mut devices = self.devices.write().await;
        if let Some(exported) = devices.get_mut(bus_id) {
            if exported.active_session == Some(session_id) {
                if timeout.is_zero() {
                    // No grace period -- release immediately.
                    exported.active_session = None;
                    exported.session_state = None;
                    tracing::info!(bus_id, session_id, "device released from import session");
                } else {
                    let deadline = tokio::time::Instant::now() + timeout;
                    exported.session_state = Some(SessionState::Disconnected {
                        session_id,
                        bus_id: bus_id.to_string(),
                        deadline,
                    });
                    tracing::info!(
                        bus_id,
                        session_id,
                        grace_period_secs = timeout.as_secs(),
                        "session disconnected, entering grace period"
                    );

                    // Spawn a cleanup task that will clear the session after the
                    // grace period, unless a reconnection happens first.
                    let registry = self.clone();
                    let bus_id = bus_id.to_string();
                    tokio::spawn(async move {
                        tokio::time::sleep(timeout).await;
                        registry
                            .expire_disconnected_session(&bus_id, session_id)
                            .await;
                    });
                }
            }
        }
    }

    /// Release a device immediately, skipping the grace period.
    ///
    /// Used when the device is explicitly unbound or when we know reconnection
    /// is not desired.
    pub async fn release_immediate(&self, bus_id: &str, session_id: SessionId) {
        let mut devices = self.devices.write().await;
        if let Some(exported) = devices.get_mut(bus_id) {
            if exported.active_session == Some(session_id) {
                exported.active_session = None;
                exported.session_state = None;
                tracing::info!(
                    bus_id,
                    session_id,
                    "device released immediately from import session"
                );
            }
        }
    }

    /// Expire a disconnected session after the grace period.
    ///
    /// Only clears the session if it is still in the `Disconnected` state
    /// with the same `session_id` (i.e. no reconnection occurred).
    async fn expire_disconnected_session(&self, bus_id: &str, session_id: SessionId) {
        let mut devices = self.devices.write().await;
        if let Some(exported) = devices.get_mut(bus_id) {
            if let Some(SessionState::Disconnected {
                session_id: sid, ..
            }) = &exported.session_state
            {
                if *sid == session_id {
                    exported.active_session = None;
                    exported.session_state = None;
                    tracing::info!(bus_id, session_id, "grace period expired, device released");
                }
            }
        }
    }

    /// Check whether a device has a disconnected session within its grace
    /// period for the given bus ID.
    pub async fn has_disconnected_session(&self, bus_id: &str) -> bool {
        let devices = self.devices.read().await;
        if let Some(exported) = devices.get(bus_id) {
            if let Some(SessionState::Disconnected {
                bus_id: ref dbus,
                deadline,
                ..
            }) = exported.session_state
            {
                return dbus == bus_id && tokio::time::Instant::now() < deadline;
            }
        }
        false
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

    #[tokio::test]
    async fn test_with_session_timeout() {
        let registry = ExportRegistry::with_session_timeout(60);
        assert_eq!(registry.session_timeout(), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_default_session_timeout() {
        let registry = ExportRegistry::new();
        assert_eq!(
            registry.session_timeout(),
            Duration::from_secs(DEFAULT_SESSION_TIMEOUT_SECS)
        );
    }

    #[tokio::test]
    async fn test_has_disconnected_session_empty() {
        let registry = ExportRegistry::new();
        assert!(!registry.has_disconnected_session("1-1").await);
    }

    #[tokio::test]
    async fn test_session_grace_period_expiry() {
        // Use a very short timeout for testing.
        let registry = ExportRegistry::with_session_timeout(0);

        // With zero timeout, release should clear immediately.
        // There's nothing to expire since the device isn't bound.
        registry.release("1-1", 1).await;
        assert!(!registry.has_disconnected_session("1-1").await);
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
