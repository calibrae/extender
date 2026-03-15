//! User-facing types for the USB/IP client.

use std::net::SocketAddr;

use extender_protocol::UsbDevice;

/// A user-friendly representation of a remote USB device available for import.
#[derive(Debug, Clone)]
pub struct RemoteDevice {
    /// The bus ID on the remote server (e.g., "1-4.2").
    pub busid: String,
    /// The sysfs path on the remote server.
    pub path: String,
    /// USB vendor ID.
    pub id_vendor: u16,
    /// USB product ID.
    pub id_product: u16,
    /// Device release number in BCD.
    pub bcd_device: u16,
    /// USB device class.
    pub device_class: u8,
    /// USB device subclass.
    pub device_subclass: u8,
    /// USB device protocol.
    pub device_protocol: u8,
    /// USB speed (1=low, 2=full, 3=high, 5=super).
    pub speed: u32,
    /// Number of interfaces on this device.
    pub num_interfaces: u8,
    /// Interface class codes.
    pub interface_classes: Vec<u8>,
}

impl From<&UsbDevice> for RemoteDevice {
    fn from(dev: &UsbDevice) -> Self {
        RemoteDevice {
            busid: dev.busid_string().to_owned(),
            path: dev.path_string().to_owned(),
            id_vendor: dev.id_vendor,
            id_product: dev.id_product,
            bcd_device: dev.bcd_device,
            device_class: dev.device_class,
            device_subclass: dev.device_subclass,
            device_protocol: dev.device_protocol,
            speed: dev.speed,
            num_interfaces: dev.num_interfaces,
            interface_classes: dev.interfaces.iter().map(|i| i.interface_class).collect(),
        }
    }
}

/// Information about a device that has been successfully attached (imported).
#[derive(Debug, Clone)]
pub struct AttachedDevice {
    /// The local VHCI port number this device is attached to.
    pub port: u32,
    /// The bus ID on the remote server.
    pub busid: String,
    /// The address of the remote server.
    pub server_addr: SocketAddr,
    /// USB vendor ID.
    pub id_vendor: u16,
    /// USB product ID.
    pub id_product: u16,
    /// USB speed.
    pub speed: u32,
}

/// Represents a device currently imported through VHCI, combining
/// kernel state with local registry data.
#[derive(Debug, Clone)]
pub struct ImportedDevice {
    /// The local VHCI port number.
    pub port: u32,
    /// Port status from the VHCI driver.
    pub status: PortStatus,
    /// USB speed.
    pub speed: u32,
    /// Device ID (busnum << 16 | devnum).
    pub devid: u32,
    /// The remote server address, if known from the local registry.
    pub server_addr: Option<SocketAddr>,
    /// The remote bus ID, if known from the local registry.
    pub busid: Option<String>,
}

/// VHCI port status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStatus {
    /// Port is free and available for use.
    Free,
    /// Port is in use (device attached).
    InUse,
    /// Unknown status value.
    Unknown(u32),
}

impl PortStatus {
    /// Parse a status value from the VHCI status file.
    pub fn from_raw(val: u32) -> Self {
        match val {
            4 => PortStatus::Free,
            6 => PortStatus::InUse,
            other => PortStatus::Unknown(other),
        }
    }

    /// Whether this port status indicates the port is available.
    pub fn is_free(&self) -> bool {
        matches!(self, PortStatus::Free)
    }
}

/// A parsed row from the VHCI status file.
#[derive(Debug, Clone)]
pub struct VhciPort {
    /// The hub type ("hs" for high-speed, "ss" for super-speed).
    pub hub: String,
    /// The port number.
    pub port: u32,
    /// The port status.
    pub status: PortStatus,
    /// USB speed.
    pub speed: u32,
    /// Device ID (busnum << 16 | devnum), 0 if free.
    pub devid: u32,
    /// Socket file descriptor, 0 if free.
    pub sockfd: u32,
    /// Local bus ID assigned by the kernel.
    pub local_busid: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use extender_protocol::{UsbDevice, UsbInterface};

    #[test]
    fn test_remote_device_from_usb_device() {
        let dev = UsbDevice {
            path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
            busid: UsbDevice::busid_from_str("1-1").unwrap(),
            busnum: 1,
            devnum: 2,
            speed: 3,
            id_vendor: 0x1234,
            id_product: 0x5678,
            bcd_device: 0x0100,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 2,
            interfaces: vec![
                UsbInterface {
                    interface_class: 0x03,
                    interface_subclass: 0x01,
                    interface_protocol: 0x02,
                    padding: 0,
                },
                UsbInterface {
                    interface_class: 0x08,
                    interface_subclass: 0x06,
                    interface_protocol: 0x50,
                    padding: 0,
                },
            ],
        };

        let remote = RemoteDevice::from(&dev);
        assert_eq!(remote.busid, "1-1");
        assert_eq!(remote.path, "/sys/devices/usb1/1-1");
        assert_eq!(remote.id_vendor, 0x1234);
        assert_eq!(remote.id_product, 0x5678);
        assert_eq!(remote.speed, 3);
        assert_eq!(remote.num_interfaces, 2);
        assert_eq!(remote.interface_classes, vec![0x03, 0x08]);
    }

    #[test]
    fn test_port_status_from_raw() {
        assert_eq!(PortStatus::from_raw(4), PortStatus::Free);
        assert_eq!(PortStatus::from_raw(6), PortStatus::InUse);
        assert_eq!(PortStatus::from_raw(99), PortStatus::Unknown(99));
        assert!(PortStatus::Free.is_free());
        assert!(!PortStatus::InUse.is_free());
    }
}
