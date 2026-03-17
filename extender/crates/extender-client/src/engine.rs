//! Client engine: orchestrates attach, detach, and status operations.
//!
//! The `ClientEngine` holds a reference to the VHCI driver and a registry
//! of imported devices. It coordinates the TCP protocol exchange with the
//! sysfs writes needed to attach/detach devices through the kernel's vhci_hcd.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

use crate::error::ClientError;
use crate::types::{AttachedDevice, ImportedDevice};

/// Registry entry for an imported device.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields read only on Linux
struct RegistryEntry {
    server_addr: SocketAddr,
    busid: String,
    id_vendor: u16,
    id_product: u16,
    speed: u32,
}

/// The client engine manages device import/export operations.
///
/// On Linux, it interfaces with the vhci_hcd kernel module via sysfs.
/// On Windows, it interfaces with the usbip-win2 UDE driver via IOCTLs.
/// On other platforms, attach/detach operations return `PlatformNotSupported`.
pub struct ClientEngine {
    /// VHCI driver instance (Linux only).
    #[cfg(target_os = "linux")]
    vhci: Box<dyn crate::vhci::VirtualHci>,

    /// VHCI driver instance (Windows only).
    #[cfg(target_os = "windows")]
    vhci: crate::vhci_windows::WindowsVhciDriver,

    /// Registry of imported devices, keyed by port number.
    #[allow(dead_code)] // Used only on Linux/Windows
    registry: Mutex<HashMap<u32, RegistryEntry>>,
}

impl ClientEngine {
    /// Create a new ClientEngine with the real VHCI driver.
    ///
    /// On Linux, this opens the vhci_hcd sysfs interface.
    /// On other platforms, the engine is created but attach/detach will
    /// return `PlatformNotSupported`.
    #[cfg(target_os = "linux")]
    pub fn new() -> Result<Self, ClientError> {
        let vhci = Box::new(crate::vhci::VhciDriver::new()?);
        Ok(ClientEngine {
            vhci,
            registry: Mutex::new(HashMap::new()),
        })
    }

    /// Create a new ClientEngine on Windows.
    ///
    /// Opens the usbip-win2 VHCI driver device for attach/detach operations.
    #[cfg(target_os = "windows")]
    pub fn new() -> Result<Self, ClientError> {
        let vhci = crate::vhci_windows::WindowsVhciDriver::new()?;
        Ok(ClientEngine {
            vhci,
            registry: Mutex::new(HashMap::new()),
        })
    }

    /// Create a new ClientEngine on unsupported platforms.
    ///
    /// Attach and detach operations will return `PlatformNotSupported`.
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    pub fn new() -> Result<Self, ClientError> {
        Ok(ClientEngine {
            registry: Mutex::new(HashMap::new()),
        })
    }

    /// Create a ClientEngine with a custom VHCI driver (for testing).
    #[cfg(target_os = "linux")]
    pub fn with_vhci(vhci: Box<dyn crate::vhci::VirtualHci>) -> Self {
        ClientEngine {
            vhci,
            registry: Mutex::new(HashMap::new()),
        }
    }

    /// Attach (import) a remote USB device.
    ///
    /// This performs the full import flow:
    /// 1. TCP connect to the server
    /// 2. Send OP_REQ_IMPORT with the given bus ID
    /// 3. Receive OP_REP_IMPORT with device info
    /// 4. Extract the raw TCP socket fd
    /// 5. Write to vhci_hcd sysfs to attach the device
    /// 6. Record the device in the local registry
    pub async fn attach_device(
        &self,
        addr: SocketAddr,
        busid: &str,
    ) -> Result<AttachedDevice, ClientError> {
        // Platform gate
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = (addr, busid);
            Err(ClientError::PlatformNotSupported)
        }

        #[cfg(target_os = "linux")]
        {
            self.attach_device_linux(addr, busid).await
        }

        #[cfg(target_os = "windows")]
        {
            self.attach_device_windows(addr, busid).await
        }
    }

    /// Linux-specific attach implementation.
    #[cfg(target_os = "linux")]
    async fn attach_device_linux(
        &self,
        addr: SocketAddr,
        busid: &str,
    ) -> Result<AttachedDevice, ClientError> {
        use std::os::unix::io::AsRawFd;
        use std::time::Duration;

        use tokio::net::TcpStream;
        use tokio::time::timeout;

        use extender_protocol::codec::{read_op_message, write_op_message};
        use extender_protocol::{OpMessage, OpReqImport};

        /// Connect timeout in seconds.
        const CONNECT_TIMEOUT_SECS: u64 = 5;

        let busid_wire = extender_protocol::UsbDevice::busid_from_str(busid)
            .map_err(|_| ClientError::InvalidBusId(busid.to_owned()))?;

        let connect_timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

        // Connect with timeout
        let stream = timeout(connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| ClientError::ConnectTimeout {
                addr,
                timeout_secs: CONNECT_TIMEOUT_SECS,
            })?
            .map_err(ClientError::Io)?;

        let (mut reader, mut writer) = stream.into_split();

        // Send OP_REQ_IMPORT
        let req = OpMessage::ReqImport(OpReqImport { busid: busid_wire });
        write_op_message(&mut writer, &req).await?;

        // Read OP_REP_IMPORT
        let reply = read_op_message(&mut reader).await?;

        let device = match reply {
            OpMessage::RepImport(rep) => {
                if rep.status != 0 {
                    return Err(ClientError::ImportRejected {
                        busid: busid.to_owned(),
                        status: rep.status,
                    });
                }
                rep.device.ok_or(ClientError::ImportMissingDevice)?
            }
            _ => {
                return Err(ClientError::Protocol(
                    extender_protocol::ProtocolError::InvalidOpCode(0),
                ));
            }
        };

        // Reunite the stream halves to get the raw fd
        let reunited = reader.reunite(writer).map_err(|e| {
            ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("failed to reunite TCP stream: {e}"),
            ))
        })?;

        let fd = reunited.as_raw_fd();
        let devid = (device.busnum << 16) | device.devnum;
        let speed = device.speed;

        // Find a free port
        let port = self.vhci.find_free_port(speed)?;

        // Attach through vhci sysfs
        self.vhci.attach(port, fd, devid, speed)?;

        tracing::info!(
            port = port,
            busid = busid,
            server = %addr,
            devid = devid,
            speed = speed,
            "device attached"
        );

        // Record in registry
        let entry = RegistryEntry {
            server_addr: addr,
            busid: busid.to_owned(),
            id_vendor: device.id_vendor,
            id_product: device.id_product,
            speed,
        };
        self.registry.lock().unwrap().insert(port, entry);

        // Transfer fd ownership to the kernel by converting to a raw fd.
        // The kernel takes ownership and will close it when the device is detached.
        use std::os::unix::io::IntoRawFd;
        let _fd = reunited.into_std().unwrap().into_raw_fd();

        Ok(AttachedDevice {
            port,
            busid: busid.to_owned(),
            server_addr: addr,
            id_vendor: device.id_vendor,
            id_product: device.id_product,
            speed,
        })
    }

    /// Windows-specific attach implementation.
    ///
    /// On Windows, the usbip-win2 driver handles TCP and USB/IP protocol
    /// entirely in kernel space. We just send an IOCTL with the server
    /// address and bus ID, and the driver does the rest.
    #[cfg(target_os = "windows")]
    async fn attach_device_windows(
        &self,
        addr: SocketAddr,
        busid: &str,
    ) -> Result<AttachedDevice, ClientError> {
        let server_ip = addr.ip().to_string();
        let server_port = addr.port();

        // The driver handles TCP connection and USB/IP protocol in kernel space.
        // We just send the IOCTL with the server address and bus ID.
        let port = self.vhci.attach(&server_ip, server_port, busid)?;

        tracing::info!(
            port = port,
            busid = busid,
            server = %addr,
            "device attached via Windows VHCI"
        );

        // Record in registry
        let entry = RegistryEntry {
            server_addr: addr,
            busid: busid.to_owned(),
            // On Windows, we don't get vendor/product info from the attach response.
            // TODO: Query device info from the driver after attach, or parse it
            // from a prior device list query.
            id_vendor: 0,
            id_product: 0,
            speed: 0,
        };
        self.registry.lock().unwrap().insert(port, entry);

        Ok(AttachedDevice {
            port,
            busid: busid.to_owned(),
            server_addr: addr,
            id_vendor: 0,
            id_product: 0,
            speed: 0,
        })
    }

    /// Detach a previously imported device by port number.
    ///
    /// Writes to the vhci_hcd detach sysfs file and removes the device
    /// from the local registry.
    pub async fn detach_device(&self, port: u32) -> Result<(), ClientError> {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = port;
            Err(ClientError::PlatformNotSupported)
        }

        #[cfg(target_os = "linux")]
        {
            // Verify the port is in use (check registry or vhci status)
            let ports = self.vhci.list_ports()?;
            let vhci_port = ports.iter().find(|p| p.port == port);
            match vhci_port {
                Some(p) if p.status.is_free() => {
                    return Err(ClientError::PortNotAttached { port });
                }
                None => {
                    return Err(ClientError::PortNotAttached { port });
                }
                _ => {}
            }

            // Detach through vhci sysfs
            self.vhci.detach(port)?;

            // Remove from registry
            self.registry.lock().unwrap().remove(&port);

            tracing::info!(port = port, "device detached");
            Ok(())
        }

        #[cfg(target_os = "windows")]
        {
            // On Windows, the driver handles validation internally.
            self.vhci.detach(port)?;

            // Remove from registry
            self.registry.lock().unwrap().remove(&port);

            tracing::info!(port = port, "device detached via Windows VHCI");
            Ok(())
        }
    }

    /// Get the list of currently imported devices.
    ///
    /// Parses the vhci_hcd status file and cross-references with the local
    /// registry to provide server address and bus ID information.
    pub fn get_imported_devices(&self) -> Result<Vec<ImportedDevice>, ClientError> {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Err(ClientError::PlatformNotSupported)
        }

        #[cfg(target_os = "linux")]
        {
            let ports = self.vhci.list_ports()?;
            let registry = self.registry.lock().unwrap();

            let imported: Vec<ImportedDevice> = ports
                .iter()
                .filter(|p| !p.status.is_free())
                .map(|p| {
                    let reg = registry.get(&p.port);
                    ImportedDevice {
                        port: p.port,
                        status: p.status,
                        speed: p.speed,
                        devid: p.devid,
                        server_addr: reg.map(|r| r.server_addr),
                        busid: reg.map(|r| r.busid.clone()),
                    }
                })
                .collect();

            Ok(imported)
        }

        #[cfg(target_os = "windows")]
        {
            let mut devices = self.vhci.list_ports()?;
            let registry = self.registry.lock().unwrap();

            // Enrich with registry data (server address and busid).
            for dev in &mut devices {
                if let Some(reg) = registry.get(&dev.port) {
                    dev.server_addr = Some(reg.server_addr);
                    dev.busid = Some(reg.busid.clone());
                }
            }

            Ok(devices)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[tokio::test]
    async fn test_attach_not_supported_on_unsupported_platform() {
        let engine = ClientEngine::new().unwrap();
        let addr: SocketAddr = "127.0.0.1:3240".parse().unwrap();
        let result = engine.attach_device(addr, "1-1").await;
        assert!(matches!(result, Err(ClientError::PlatformNotSupported)));
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[tokio::test]
    async fn test_detach_not_supported_on_unsupported_platform() {
        let engine = ClientEngine::new().unwrap();
        let result = engine.detach_device(0).await;
        assert!(matches!(result, Err(ClientError::PlatformNotSupported)));
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[test]
    fn test_get_imported_not_supported_on_unsupported_platform() {
        let engine = ClientEngine::new().unwrap();
        let result = engine.get_imported_devices();
        assert!(matches!(result, Err(ClientError::PlatformNotSupported)));
    }

    // Cross-platform tests for Windows IOCTL code calculations.
    // These verify the CTL_CODE macro logic without requiring Windows.

    /// Compute a buffered IOCTL code for FILE_DEVICE_UNKNOWN (0x22).
    /// Mirrors the implementation in vhci_windows.rs.
    const fn ctl_code(function: u32) -> u32 {
        (0x22 << 16) | (function << 2)
    }

    #[test]
    fn test_windows_ioctl_plugin_hardware() {
        assert_eq!(ctl_code(1), 0x0022_0004);
    }

    #[test]
    fn test_windows_ioctl_unplug_hardware() {
        assert_eq!(ctl_code(2), 0x0022_0008);
    }

    #[test]
    fn test_windows_ioctl_get_imported() {
        assert_eq!(ctl_code(3), 0x0022_000C);
    }

    #[test]
    fn test_windows_ctl_code_formula() {
        // Verify the formula: (device_type << 16) | (function << 2)
        // with device_type = FILE_DEVICE_UNKNOWN = 0x22
        for func in 0..16u32 {
            let code = ctl_code(func);
            assert_eq!(code >> 16, 0x22, "device type should be 0x22");
            assert_eq!((code >> 2) & 0xFFF, func, "function number mismatch");
            assert_eq!(code & 0x3, 0, "METHOD_BUFFERED should be 0");
        }
    }
}
