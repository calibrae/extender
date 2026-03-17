//! Windows VHCI driver interface using the usbip-win2 UDE driver.
//!
//! The usbip-win2 driver handles TCP connections, USB/IP protocol, and URB
//! forwarding entirely in kernel space. This module only sends IOCTLs to
//! `\\.\usbip_vhci` to attach, detach, and list imported devices.
//!
//! This module is only compiled on Windows targets.

#![cfg(target_os = "windows")]

use std::io;
use std::mem;
use std::net::SocketAddr;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::error::ClientError;
use crate::types::{ImportedDevice, PortStatus};

// ---------------------------------------------------------------------------
// IOCTL codes
// ---------------------------------------------------------------------------

/// Compute a buffered IOCTL code for `FILE_DEVICE_UNKNOWN` (0x22).
///
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, function, METHOD_BUFFERED, FILE_ANY_ACCESS)`
const fn ctl_code(function: u32) -> u32 {
    (0x22 << 16) | (function << 2)
}

/// Attach a remote USB device through the VHCI driver.
pub const IOCTL_PLUGIN_HARDWARE: u32 = ctl_code(1);

/// Detach a previously attached device.
pub const IOCTL_UNPLUG_HARDWARE: u32 = ctl_code(2);

/// Query the list of currently imported devices.
pub const IOCTL_GET_IMPORTED: u32 = ctl_code(3);

// ---------------------------------------------------------------------------
// Device path
// ---------------------------------------------------------------------------

/// Win32 device path for the usbip-win2 VHCI driver.
const VHCI_DEVICE_PATH: &str = r"\\.\usbip_vhci";

// ---------------------------------------------------------------------------
// IOCTL structures
// ---------------------------------------------------------------------------

/// Maximum lengths for wire strings in the plugin request.
const MAX_HOST_LEN: usize = 256;
const MAX_SERVICE_LEN: usize = 32;
const MAX_BUSID_LEN: usize = 32;

/// Input buffer for `IOCTL_PLUGIN_HARDWARE`.
///
/// TODO: Verify this layout against the actual usbip-win2 driver headers
/// (`vhci_ioctl.h`). The driver may use a different struct packing or
/// field order. The current layout is based on the usbip-win2 documentation.
#[repr(C, packed)]
#[derive(Clone)]
pub struct PluginHardwareRequest {
    /// Remote server hostname or IP address, null-terminated ASCII.
    pub host: [u8; MAX_HOST_LEN],
    /// Service port string (e.g., "3240"), null-terminated ASCII.
    pub service: [u8; MAX_SERVICE_LEN],
    /// Bus ID string (e.g., "1-2.4"), null-terminated ASCII.
    pub busid: [u8; MAX_BUSID_LEN],
}

impl PluginHardwareRequest {
    /// Build a plugin request from the given server address and bus ID.
    pub fn new(server: &str, port: u16, busid: &str) -> Result<Self, ClientError> {
        let mut req = PluginHardwareRequest {
            host: [0u8; MAX_HOST_LEN],
            service: [0u8; MAX_SERVICE_LEN],
            busid: [0u8; MAX_BUSID_LEN],
        };

        let host_bytes = server.as_bytes();
        if host_bytes.len() >= MAX_HOST_LEN {
            return Err(ClientError::VhciNotAvailable {
                reason: format!(
                    "hostname too long: {} bytes (max {})",
                    host_bytes.len(),
                    MAX_HOST_LEN - 1
                ),
            });
        }
        req.host[..host_bytes.len()].copy_from_slice(host_bytes);

        let port_str = port.to_string();
        let port_bytes = port_str.as_bytes();
        if port_bytes.len() >= MAX_SERVICE_LEN {
            return Err(ClientError::VhciNotAvailable {
                reason: format!(
                    "port string too long: {} bytes (max {})",
                    port_bytes.len(),
                    MAX_SERVICE_LEN - 1
                ),
            });
        }
        req.service[..port_bytes.len()].copy_from_slice(port_bytes);

        let busid_bytes = busid.as_bytes();
        if busid_bytes.len() >= MAX_BUSID_LEN {
            return Err(ClientError::VhciNotAvailable {
                reason: format!(
                    "busid too long: {} bytes (max {})",
                    busid_bytes.len(),
                    MAX_BUSID_LEN - 1
                ),
            });
        }
        req.busid[..busid_bytes.len()].copy_from_slice(busid_bytes);

        Ok(req)
    }
}

/// Output buffer for `IOCTL_PLUGIN_HARDWARE`.
///
/// TODO: Verify against driver headers. The driver returns the assigned port
/// number for the newly attached device.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PluginHardwareReply {
    /// Port number assigned to the attached device.
    pub port: i32,
}

/// Input buffer for `IOCTL_UNPLUG_HARDWARE`.
///
/// TODO: Verify against driver headers.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct UnplugHardwareRequest {
    /// Port number of the device to detach. Use -1 to detach all.
    pub port: i32,
}

/// A single entry in the imported-devices response from the driver.
///
/// TODO: Verify field layout and sizes against `vhci_ioctl.h`. The actual
/// driver struct may differ in field order, alignment, or total size.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ImportedDeviceEntry {
    /// Port number on the VHCI controller.
    pub port: i32,
    /// Device status (0 = free, non-zero = in use).
    pub status: u32,
    /// USB speed of the device.
    pub speed: u32,
    /// Vendor ID.
    pub vendor_id: u16,
    /// Product ID.
    pub product_id: u16,
    /// Device ID (busnum << 16 | devnum).
    pub devid: u32,
}

// ---------------------------------------------------------------------------
// Driver handle wrapper
// ---------------------------------------------------------------------------

/// RAII wrapper around a Win32 HANDLE for the VHCI device.
struct VhciHandle(HANDLE);

impl VhciHandle {
    fn as_raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for VhciHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && self.0 != 0 {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

// Safety: The handle is used behind &self with synchronized IOCTLs.
unsafe impl Send for VhciHandle {}
unsafe impl Sync for VhciHandle {}

// ---------------------------------------------------------------------------
// WindowsVhciDriver
// ---------------------------------------------------------------------------

/// Windows VHCI driver interface.
///
/// Opens the `\\.\usbip_vhci` device and provides methods to attach, detach,
/// and list imported USB devices via IOCTLs.
pub struct WindowsVhciDriver {
    handle: VhciHandle,
}

impl WindowsVhciDriver {
    /// Open the VHCI device.
    ///
    /// Returns an error if the usbip-win2 driver is not installed or the
    /// device cannot be opened.
    pub fn new() -> Result<Self, ClientError> {
        let handle = open_vhci_device()?;
        Ok(WindowsVhciDriver {
            handle: VhciHandle(handle),
        })
    }

    /// Attach a remote USB device.
    ///
    /// Sends `IOCTL_PLUGIN_HARDWARE` to the driver with the server address
    /// and bus ID. The driver establishes the TCP connection and USB/IP
    /// session in kernel space.
    ///
    /// Returns the port number assigned by the driver.
    pub fn attach(&self, server: &str, port: u16, busid: &str) -> Result<u32, ClientError> {
        let req = PluginHardwareRequest::new(server, port, busid)?;
        let mut reply = PluginHardwareReply::default();
        let mut bytes_returned: u32 = 0;

        let success = unsafe {
            DeviceIoControl(
                self.handle.as_raw(),
                IOCTL_PLUGIN_HARDWARE,
                &req as *const _ as *const _,
                mem::size_of::<PluginHardwareRequest>() as u32,
                &mut reply as *mut _ as *mut _,
                mem::size_of::<PluginHardwareReply>() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if success == 0 {
            return Err(ClientError::Io(io::Error::last_os_error()));
        }

        if reply.port < 0 {
            return Err(ClientError::VhciNotAvailable {
                reason: format!("driver returned negative port: {}", reply.port),
            });
        }

        tracing::info!(
            port = reply.port,
            server = server,
            busid = busid,
            "device attached via Windows VHCI"
        );

        Ok(reply.port as u32)
    }

    /// Detach a device by port number.
    ///
    /// Sends `IOCTL_UNPLUG_HARDWARE` to the driver.
    pub fn detach(&self, port: u32) -> Result<(), ClientError> {
        let req = UnplugHardwareRequest { port: port as i32 };
        let mut bytes_returned: u32 = 0;

        let success = unsafe {
            DeviceIoControl(
                self.handle.as_raw(),
                IOCTL_UNPLUG_HARDWARE,
                &req as *const _ as *const _,
                mem::size_of::<UnplugHardwareRequest>() as u32,
                std::ptr::null_mut(),
                0,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if success == 0 {
            return Err(ClientError::Io(io::Error::last_os_error()));
        }

        tracing::info!(port = port, "device detached via Windows VHCI");
        Ok(())
    }

    /// List all currently imported devices.
    ///
    /// Sends `IOCTL_GET_IMPORTED` and parses the returned buffer.
    pub fn list_ports(&self) -> Result<Vec<ImportedDevice>, ClientError> {
        // Allocate a buffer large enough for a reasonable number of devices.
        // TODO: Verify the actual response format from the driver. The driver
        // may prefix the buffer with a count or use a different layout.
        const MAX_DEVICES: usize = 128;
        let buf_size = MAX_DEVICES * mem::size_of::<ImportedDeviceEntry>();
        let mut buf = vec![0u8; buf_size];
        let mut bytes_returned: u32 = 0;

        let success = unsafe {
            DeviceIoControl(
                self.handle.as_raw(),
                IOCTL_GET_IMPORTED,
                std::ptr::null(),
                0,
                buf.as_mut_ptr() as *mut _,
                buf_size as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if success == 0 {
            return Err(ClientError::Io(io::Error::last_os_error()));
        }

        let entry_size = mem::size_of::<ImportedDeviceEntry>();
        let count = bytes_returned as usize / entry_size;
        let mut devices = Vec::with_capacity(count);

        for i in 0..count {
            let offset = i * entry_size;
            if offset + entry_size > bytes_returned as usize {
                break;
            }
            let entry: ImportedDeviceEntry =
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset) as *const _) };

            let status = if entry.status == 0 {
                PortStatus::Free
            } else {
                PortStatus::InUse
            };

            devices.push(ImportedDevice {
                port: entry.port as u32,
                status,
                speed: entry.speed,
                devid: entry.devid,
                server_addr: None,
                busid: None,
            });
        }

        Ok(devices)
    }
}

// ---------------------------------------------------------------------------
// Helper: open the VHCI device via CreateFileW
// ---------------------------------------------------------------------------

/// Open the `\\.\usbip_vhci` device with read/write access.
fn open_vhci_device() -> Result<HANDLE, ClientError> {
    let wide_path: Vec<u16> = VHCI_DEVICE_PATH
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null::<SECURITY_ATTRIBUTES>(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            0,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        return Err(ClientError::VhciNotAvailable {
            reason: format!(
                "failed to open {}: {} (is usbip-win2 driver installed?)",
                VHCI_DEVICE_PATH, err
            ),
        });
    }

    Ok(handle)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify IOCTL code computation matches expected values.
    #[test]
    fn test_ioctl_codes() {
        // CTL_CODE(0x22, 1, 0, 0) = 0x00220004
        assert_eq!(IOCTL_PLUGIN_HARDWARE, 0x0022_0004);
        // CTL_CODE(0x22, 2, 0, 0) = 0x00220008
        assert_eq!(IOCTL_UNPLUG_HARDWARE, 0x0022_0008);
        // CTL_CODE(0x22, 3, 0, 0) = 0x0022000C
        assert_eq!(IOCTL_GET_IMPORTED, 0x0022_000C);
    }

    /// Verify PluginHardwareRequest is constructed correctly.
    #[test]
    fn test_plugin_request_construction() {
        let req = PluginHardwareRequest::new("192.168.1.100", 3240, "1-2.4").unwrap();

        // Check host field
        let host_str = std::str::from_utf8(&req.host)
            .unwrap()
            .trim_end_matches('\0');
        assert_eq!(host_str, "192.168.1.100");

        // Check service field
        let service_str = std::str::from_utf8(&req.service)
            .unwrap()
            .trim_end_matches('\0');
        assert_eq!(service_str, "3240");

        // Check busid field
        let busid_str = std::str::from_utf8(&req.busid)
            .unwrap()
            .trim_end_matches('\0');
        assert_eq!(busid_str, "1-2.4");
    }

    /// Verify that overly long hostnames are rejected.
    #[test]
    fn test_plugin_request_host_too_long() {
        let long_host = "a".repeat(MAX_HOST_LEN);
        let result = PluginHardwareRequest::new(&long_host, 3240, "1-1");
        assert!(result.is_err());
    }

    /// Verify struct sizes are reasonable.
    #[test]
    fn test_struct_sizes() {
        assert_eq!(
            std::mem::size_of::<PluginHardwareRequest>(),
            MAX_HOST_LEN + MAX_SERVICE_LEN + MAX_BUSID_LEN,
        );
        assert_eq!(std::mem::size_of::<PluginHardwareReply>(), 4);
        assert_eq!(std::mem::size_of::<UnplugHardwareRequest>(), 4);
    }

    /// Test that the driver can be opened. This test requires the usbip-win2
    /// driver to be installed and will be skipped on CI.
    #[test]
    #[ignore]
    fn test_open_vhci_device() {
        let driver = WindowsVhciDriver::new();
        assert!(
            driver.is_ok(),
            "failed to open VHCI device: {:?}",
            driver.err()
        );
    }

    /// Test attach + detach round-trip. Requires the driver and a real server.
    #[test]
    #[ignore]
    fn test_attach_detach() {
        let driver = WindowsVhciDriver::new().expect("failed to open VHCI device");
        let port = driver
            .attach("127.0.0.1", 3240, "1-1")
            .expect("failed to attach");
        assert!(port < 128, "unexpected port number: {port}");
        driver.detach(port).expect("failed to detach");
    }

    /// Test listing imported devices. Requires the driver.
    #[test]
    #[ignore]
    fn test_list_ports() {
        let driver = WindowsVhciDriver::new().expect("failed to open VHCI device");
        let devices = driver.list_ports().expect("failed to list ports");
        // Just verify we get a list without errors
        tracing::info!("imported devices: {devices:?}");
    }
}
