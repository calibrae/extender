//! USB device handle management.
//!
//! Provides [`ManagedDevice`] which wraps a `rusb::DeviceHandle` with
//! automatic interface claiming/release and kernel driver detachment.

use std::sync::Arc;

use rusb::UsbContext;

use crate::device::LocalUsbDevice;
use crate::error::ServerError;

/// A managed USB device handle that automatically releases interfaces on drop.
///
/// This struct wraps a `rusb::DeviceHandle<rusb::Context>` and tracks which
/// interfaces have been claimed. When dropped, it releases all claimed
/// interfaces and closes the handle.
///
/// Use [`ManagedDevice::open`] to create a new managed handle.
pub struct ManagedDevice {
    handle: rusb::DeviceHandle<rusb::Context>,
    claimed_interfaces: Vec<u8>,
    bus_id: String,
}

impl std::fmt::Debug for ManagedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedDevice")
            .field("bus_id", &self.bus_id)
            .field("claimed_interfaces", &self.claimed_interfaces)
            .finish()
    }
}

impl ManagedDevice {
    /// Open a USB device by bus ID and claim all its interfaces.
    ///
    /// This will:
    /// 1. Find the device matching the given bus ID in the device list.
    /// 2. Open the device handle.
    /// 3. Enable auto-detach of kernel drivers (on platforms that support it).
    /// 4. Claim all interfaces of the active configuration.
    ///
    /// Returns the managed device wrapped in an `Arc` for thread-safe sharing.
    pub fn open(devices: &[LocalUsbDevice], bus_id: &str) -> Result<Arc<Self>, ServerError> {
        let local = devices.iter().find(|d| d.bus_id == bus_id).ok_or_else(|| {
            ServerError::DeviceNotFound {
                bus_id: bus_id.to_string(),
            }
        })?;

        let context = rusb::Context::new().map_err(ServerError::UsbContextInit)?;
        let usb_devices = context.devices().map_err(ServerError::Enumeration)?;

        // Find the matching rusb device by bus number and address.
        let rusb_device = usb_devices
            .iter()
            .find(|d| d.bus_number() == local.bus_number && d.address() == local.device_address)
            .ok_or_else(|| ServerError::DeviceNotFound {
                bus_id: bus_id.to_string(),
            })?;

        let handle = rusb_device.open().map_err(|e| match e {
            rusb::Error::Access => ServerError::DeviceInUse {
                bus_id: bus_id.to_string(),
            },
            rusb::Error::Busy => ServerError::DeviceInUse {
                bus_id: bus_id.to_string(),
            },
            other => ServerError::OpenDevice {
                bus_id: bus_id.to_string(),
                source: other,
            },
        })?;

        // Enable auto-detach of kernel drivers. This is a no-op on macOS
        // where libusb always detaches kernel drivers automatically, but
        // it's important on Linux.
        if let Err(e) = handle.set_auto_detach_kernel_driver(true) {
            // Not all platforms support this; log but don't fail.
            tracing::debug!(bus_id, "auto-detach kernel driver not supported: {}", e);
        }

        // Claim all interfaces.
        let mut claimed_interfaces = Vec::new();
        let interface_numbers: Vec<u8> = local
            .interfaces
            .iter()
            .map(|i| i.interface_number)
            .collect();

        for iface_num in &interface_numbers {
            handle.claim_interface(*iface_num).map_err(|e| match e {
                rusb::Error::Busy => ServerError::DeviceInUse {
                    bus_id: bus_id.to_string(),
                },
                other => ServerError::ClaimInterface {
                    bus_id: bus_id.to_string(),
                    interface: *iface_num,
                    source: other,
                },
            })?;
            claimed_interfaces.push(*iface_num);
            tracing::debug!(bus_id, interface = iface_num, "claimed interface");
        }

        tracing::info!(
            bus_id,
            interfaces = ?claimed_interfaces,
            "device opened and interfaces claimed"
        );

        Ok(Arc::new(ManagedDevice {
            handle,
            claimed_interfaces,
            bus_id: bus_id.to_string(),
        }))
    }

    /// Get a reference to the underlying rusb device handle.
    ///
    /// This is used by the transfer module to execute USB transfers.
    pub fn handle(&self) -> &rusb::DeviceHandle<rusb::Context> {
        &self.handle
    }

    /// Get the bus ID of this device.
    pub fn bus_id(&self) -> &str {
        &self.bus_id
    }

    /// Get the list of claimed interface numbers.
    pub fn claimed_interfaces(&self) -> &[u8] {
        &self.claimed_interfaces
    }
}

impl Drop for ManagedDevice {
    fn drop(&mut self) {
        for iface in &self.claimed_interfaces {
            if let Err(e) = self.handle.release_interface(*iface) {
                tracing::warn!(
                    bus_id = %self.bus_id,
                    interface = iface,
                    "failed to release interface: {}",
                    e
                );
            } else {
                tracing::debug!(
                    bus_id = %self.bus_id,
                    interface = iface,
                    "released interface"
                );
            }
        }
        tracing::info!(bus_id = %self.bus_id, "device handle closed");
    }
}

// ManagedDevice is Send + Sync because rusb::DeviceHandle<rusb::Context>
// is Send + Sync (rusb 0.9 with rusb::Context).
// The Arc wrapper in open() provides the thread-safe sharing.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{LocalUsbDevice, LocalUsbInterface};

    fn make_test_device(bus_id: &str) -> LocalUsbDevice {
        LocalUsbDevice {
            bus_number: 1,
            device_address: 1,
            vendor_id: 0x1234,
            product_id: 0x5678,
            manufacturer: None,
            product: None,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            bcd_device: 0x0100,
            bus_id: bus_id.to_string(),
            speed: 3,
            num_configurations: 1,
            interfaces: vec![LocalUsbInterface {
                interface_number: 0,
                interface_class: 0x03,
                interface_subclass: 0,
                interface_protocol: 0,
            }],
            port_numbers: vec![1],
        }
    }

    #[test]
    fn test_open_device_not_found() {
        let devices = vec![make_test_device("1-1")];
        let result = ManagedDevice::open(&devices, "99-99");
        assert!(matches!(result, Err(ServerError::DeviceNotFound { .. })));
    }

    #[test]
    #[ignore]
    fn test_open_device_live() {
        // This test requires actual USB devices.
        let devices = crate::device::enumerate_devices().unwrap();
        if let Some(dev) = devices.first() {
            println!("Attempting to open device: {}", dev.bus_id);
            match ManagedDevice::open(&devices, &dev.bus_id) {
                Ok(managed) => {
                    println!(
                        "Opened device {} with {} interfaces",
                        managed.bus_id(),
                        managed.claimed_interfaces().len()
                    );
                    // Drop will release interfaces.
                }
                Err(e) => {
                    println!("Failed to open device (expected on CI): {}", e);
                }
            }
        }
    }
}
