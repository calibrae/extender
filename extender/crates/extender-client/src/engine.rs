//! Client engine: orchestrates attach, detach, and status operations.
//!
//! The `ClientEngine` holds a reference to the VHCI driver and a registry
//! of imported devices. It coordinates the TCP protocol exchange with the
//! sysfs writes needed to attach/detach devices through the kernel's vhci_hcd.

use std::collections::HashMap;
use std::net::SocketAddr;
#[cfg(target_os = "macos")]
use std::sync::Arc;
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
/// On macOS, it interfaces with the ExtenderDriver DriverKit extension via IOKit.
/// On other platforms, attach/detach operations return `PlatformNotSupported`.
pub struct ClientEngine {
    /// VHCI driver instance (Linux only).
    #[cfg(target_os = "linux")]
    vhci: Box<dyn crate::vhci::VirtualHci>,

    /// VHCI driver instance (Windows only).
    #[cfg(target_os = "windows")]
    vhci: crate::vhci_windows::WindowsVhciDriver,

    /// VHCI driver instance (macOS only).
    ///
    /// Wrapped in `Arc` so the per-device URB forwarding task can hold a
    /// clone for the lifetime of the attachment.
    #[cfg(target_os = "macos")]
    vhci: Arc<crate::vhci_macos::MacOSVhciDriver>,

    /// Registry of imported devices, keyed by port number.
    #[allow(dead_code)] // Used only on Linux/Windows/macOS
    registry: Mutex<HashMap<u32, RegistryEntry>>,

    /// macOS: per-device URB forwarding task handles, keyed by devid.
    /// On detach we abort the handle so the task tears down its TCP halves.
    #[cfg(target_os = "macos")]
    urb_tasks: Mutex<HashMap<u32, tokio::task::JoinHandle<()>>>,
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

    /// Create a new ClientEngine on macOS.
    ///
    /// Opens the ExtenderDriver DriverKit extension for attach/detach operations.
    #[cfg(target_os = "macos")]
    pub fn new() -> Result<Self, ClientError> {
        let vhci = Arc::new(crate::vhci_macos::MacOSVhciDriver::new()?);
        Ok(ClientEngine {
            vhci,
            registry: Mutex::new(HashMap::new()),
            urb_tasks: Mutex::new(HashMap::new()),
        })
    }

    /// Create a new ClientEngine on unsupported platforms.
    ///
    /// Attach and detach operations will return `PlatformNotSupported`.
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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

        #[cfg(target_os = "macos")]
        {
            self.attach_device_macos(addr, busid).await
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

    /// macOS-specific attach implementation.
    ///
    /// On macOS, the client handles the USB/IP protocol in userspace and
    /// forwards URBs to/from the DriverKit extension via IOKit UserClient calls.
    #[cfg(target_os = "macos")]
    async fn attach_device_macos(
        &self,
        addr: SocketAddr,
        busid: &str,
    ) -> Result<AttachedDevice, ClientError> {
        use std::time::Duration;

        use tokio::net::TcpStream;
        use tokio::time::timeout;

        use extender_protocol::codec::{read_op_message, write_op_message};
        use extender_protocol::{OpMessage, OpReqImport};

        use crate::vhci_macos::{device_type_from_usb, CONFIG_DEVICE_DESCRIPTOR};

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

        // Determine device type from USB descriptors.
        let interface_classes: Vec<u8> =
            device.interfaces.iter().map(|i| i.interface_class).collect();
        let device_type = device_type_from_usb(device.device_class, &interface_classes);

        // Use devid as the virtual device identifier.
        let devid = (device.busnum << 16) | device.devnum;
        let speed = device.speed;

        // Create the virtual device in the DriverKit extension.
        self.vhci.create_device(device_type, devid)?;

        // Configure with the USB device descriptor.
        // Build a minimal 18-byte USB device descriptor from the protocol data.
        let desc = build_device_descriptor(&device);
        if let Err(e) = self.vhci.configure_device(devid, CONFIG_DEVICE_DESCRIPTOR, &desc) {
            // Clean up on failure.
            let _ = self.vhci.destroy_device(devid);
            return Err(e);
        }

        tracing::info!(
            devid = devid,
            busid = busid,
            server = %addr,
            device_type = device_type,
            speed = speed,
            "device attached via macOS DriverKit"
        );

        // Record in registry. We use devid as the "port" on macOS since there
        // are no fixed VHCI port numbers — the driver assigns device IDs.
        let entry = RegistryEntry {
            server_addr: addr,
            busid: busid.to_owned(),
            id_vendor: device.id_vendor,
            id_product: device.id_product,
            speed,
        };
        self.registry.lock().unwrap().insert(devid, entry);

        // Spawn the URB forwarding task. It owns the split TCP halves and a
        // clone of the driver handle, and runs until the TCP stream closes,
        // an unrecoverable error fires, or it gets aborted on detach.
        let vhci = Arc::clone(&self.vhci);
        let handle = tokio::spawn(async move {
            urb_forward_loop(vhci, devid, reader, writer).await;
        });
        self.urb_tasks.lock().unwrap().insert(devid, handle);

        Ok(AttachedDevice {
            port: devid,
            busid: busid.to_owned(),
            server_addr: addr,
            id_vendor: device.id_vendor,
            id_product: device.id_product,
            speed,
        })
    }

    /// Detach a previously imported device by port number.
    ///
    /// Writes to the vhci_hcd detach sysfs file and removes the device
    /// from the local registry.
    pub async fn detach_device(&self, port: u32) -> Result<(), ClientError> {
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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

        #[cfg(target_os = "macos")]
        {
            // On macOS, port is the device ID.
            if !self.registry.lock().unwrap().contains_key(&port) {
                return Err(ClientError::PortNotAttached { port });
            }

            // Stop the URB forwarding task first so it doesn't race the
            // driver destroy call with in-flight IOKit calls.
            if let Some(handle) = self.urb_tasks.lock().unwrap().remove(&port) {
                handle.abort();
            }

            self.vhci.destroy_device(port)?;

            // Remove from registry
            self.registry.lock().unwrap().remove(&port);

            tracing::info!(devid = port, "device detached via macOS DriverKit");
            Ok(())
        }
    }

    /// Get the list of currently imported devices.
    ///
    /// Parses the vhci_hcd status file and cross-references with the local
    /// registry to provide server address and bus ID information.
    pub fn get_imported_devices(&self) -> Result<Vec<ImportedDevice>, ClientError> {
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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

        #[cfg(target_os = "macos")]
        {
            use crate::types::PortStatus;

            let registry = self.registry.lock().unwrap();

            let imported: Vec<ImportedDevice> = registry
                .iter()
                .map(|(&devid, reg)| ImportedDevice {
                    port: devid,
                    status: PortStatus::InUse,
                    speed: reg.speed,
                    devid,
                    server_addr: Some(reg.server_addr),
                    busid: Some(reg.busid.clone()),
                })
                .collect();

            Ok(imported)
        }
    }
}

/// Build a standard 18-byte USB device descriptor from protocol device info.
///
/// This is used on macOS to configure the virtual device with the USB device
/// descriptor received from the remote server.
#[cfg(target_os = "macos")]
fn build_device_descriptor(dev: &extender_protocol::UsbDevice) -> [u8; 18] {
    let mut desc = [0u8; 18];
    desc[0] = 18; // bLength
    desc[1] = 0x01; // bDescriptorType = DEVICE
    // bcdUSB: assume USB 2.0 unless super-speed
    let bcd_usb: u16 = if dev.speed >= 5 { 0x0300 } else { 0x0200 };
    desc[2] = bcd_usb as u8;
    desc[3] = (bcd_usb >> 8) as u8;
    desc[4] = dev.device_class;
    desc[5] = dev.device_subclass;
    desc[6] = dev.device_protocol;
    desc[7] = 64; // bMaxPacketSize0
    desc[8] = dev.id_vendor as u8;
    desc[9] = (dev.id_vendor >> 8) as u8;
    desc[10] = dev.id_product as u8;
    desc[11] = (dev.id_product >> 8) as u8;
    desc[12] = dev.bcd_device as u8;
    desc[13] = (dev.bcd_device >> 8) as u8;
    desc[14] = 0; // iManufacturer
    desc[15] = 0; // iProduct
    desc[16] = 0; // iSerialNumber
    desc[17] = dev.num_configurations;
    desc
}

/// URB forwarding loop for a single attached macOS device.
///
/// READ side: drain `RetSubmit` responses from the server and inject them into
/// the dext via `complete_request`. WRITE side: poll the dext for pending
/// host->device requests and serialize them as `CmdSubmit` to the server.
///
/// The loop terminates when the TCP read end returns an error (e.g., EOF) or
/// when the task is aborted by detach. It is intentionally minimal: no
/// CMD_UNLINK, no isochronous, no clever direction inference. Runtime
/// correctness will be exercised once the dext is signed and installed.
#[cfg(target_os = "macos")]
async fn urb_forward_loop(
    vhci: Arc<crate::vhci_macos::MacOSVhciDriver>,
    devid: u32,
    mut reader: tokio::net::tcp::OwnedReadHalf,
    mut writer: tokio::net::tcp::OwnedWriteHalf,
) {
    use std::time::Duration;

    use bytes::Bytes;
    use extender_protocol::codec::{read_urb_message, write_urb_message};
    use extender_protocol::urb::{
        CmdSubmit, UsbipHeaderBasic, NON_ISO_PACKETS_SENTINEL,
    };
    use extender_protocol::{Command, UrbMessage};

    // seqnum -> dext request_id, so we can pair RetSubmit back to the dext.
    let mut pending: HashMap<u32, u32> = HashMap::new();
    let mut next_seqnum: u32 = 1;

    // Poll the dext for new host->device requests at this cadence. 5ms is a
    // reasonable starting point; HID/storage latencies dominate well above
    // this floor.
    let mut tick = tokio::time::interval(Duration::from_millis(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // ── READ side: a RetSubmit (or other URB) arrived from the server.
            res = read_urb_message(&mut reader) => {
                match res {
                    Ok(UrbMessage::RetSubmit(ret)) => {
                        let seqnum = ret.header.seqnum;
                        let req_id = match pending.remove(&seqnum) {
                            Some(id) => id,
                            None => {
                                tracing::warn!(
                                    devid, seqnum,
                                    "RetSubmit for unknown seqnum; dropping"
                                );
                                continue;
                            }
                        };

                        // IOReturn: 0 = kIOReturnSuccess, anything else =
                        // kIOReturnError (0xE00002BC). The dext maps these
                        // back to USB-layer statuses as needed.
                        let status = if ret.status == 0 {
                            0
                        } else {
                            0xE00002BCu32 as i32
                        };

                        if let Err(e) = vhci.complete_request(
                            devid,
                            req_id,
                            status,
                            &ret.transfer_buffer,
                        ) {
                            tracing::warn!(
                                devid, req_id, error = ?e,
                                "complete_request failed"
                            );
                        }
                    }
                    Ok(UrbMessage::RetUnlink(_)) => {
                        // Unlink not implemented yet; ignore so we stay
                        // protocol-tolerant if the server emits one.
                        tracing::debug!(devid, "RetUnlink ignored");
                    }
                    Ok(other) => {
                        tracing::warn!(
                            devid,
                            "unexpected URB from server: {:?}",
                            std::mem::discriminant(&other)
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            devid, error = ?e,
                            "URB read failed; exiting forward loop"
                        );
                        break;
                    }
                }
            }

            // ── WRITE side: poll the dext for queued host->device requests.
            _ = tick.tick() => {
                let pending_req = match vhci.get_pending_output(devid) {
                    Ok(Some(req)) => req,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(
                            devid, error = ?e,
                            "get_pending_output failed; continuing"
                        );
                        continue;
                    }
                };

                let seqnum = next_seqnum;
                next_seqnum = next_seqnum.wrapping_add(1);
                pending.insert(seqnum, pending_req.request_id);

                // Direction: bit 7 of the endpoint address (USB convention).
                // 1 = IN (device→host), 0 = OUT (host→device).
                let direction = u32::from((pending_req.endpoint & 0x80) != 0);
                let ep = pending_req.endpoint & 0x0F;

                // Minimal model: ship the full pending payload as the
                // transfer buffer; leave setup zeroed. Refinement to extract
                // the 8-byte SETUP for control transfers comes once we wire
                // the dext's request_type through.
                let buf_len = pending_req.data.len() as u32;
                let cmd = CmdSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::CmdSubmit as u32,
                        seqnum,
                        devid,
                        direction,
                        ep,
                    },
                    transfer_flags: 0,
                    transfer_buffer_length: buf_len,
                    start_frame: 0,
                    number_of_packets: NON_ISO_PACKETS_SENTINEL,
                    interval: 0,
                    setup: [0u8; 8],
                    transfer_buffer: Bytes::from(pending_req.data),
                    iso_packet_descriptors: Vec::new(),
                };

                if let Err(e) = write_urb_message(
                    &mut writer,
                    &UrbMessage::CmdSubmit(cmd),
                ).await {
                    tracing::error!(
                        devid, error = ?e,
                        "URB write failed; exiting forward loop"
                    );
                    // Drop the entry we just inserted — no response will
                    // ever land.
                    pending.remove(&seqnum);
                    break;
                }
            }
        }
    }

    tracing::info!(devid, "URB forward loop exited");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    #[tokio::test]
    async fn test_attach_not_supported_on_unsupported_platform() {
        let engine = ClientEngine::new().unwrap();
        let addr: SocketAddr = "127.0.0.1:3240".parse().unwrap();
        let result = engine.attach_device(addr, "1-1").await;
        assert!(matches!(result, Err(ClientError::PlatformNotSupported)));
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    #[tokio::test]
    async fn test_detach_not_supported_on_unsupported_platform() {
        let engine = ClientEngine::new().unwrap();
        let result = engine.detach_device(0).await;
        assert!(matches!(result, Err(ClientError::PlatformNotSupported)));
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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

    // Cross-platform tests for macOS build_device_descriptor and device type detection.

    #[cfg(target_os = "macos")]
    #[test]
    fn test_build_device_descriptor_high_speed() {
        let dev = extender_protocol::UsbDevice {
            path: extender_protocol::UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
            busid: extender_protocol::UsbDevice::busid_from_str("1-1").unwrap(),
            busnum: 1,
            devnum: 2,
            speed: 3, // high-speed
            id_vendor: 0x1234,
            id_product: 0x5678,
            bcd_device: 0x0100,
            device_class: 0x03,
            device_subclass: 0x01,
            device_protocol: 0x02,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 0,
            interfaces: vec![],
        };

        let desc = build_device_descriptor(&dev);
        assert_eq!(desc[0], 18); // bLength
        assert_eq!(desc[1], 0x01); // bDescriptorType
        assert_eq!(desc[2], 0x00); // bcdUSB low
        assert_eq!(desc[3], 0x02); // bcdUSB high (USB 2.0)
        assert_eq!(desc[4], 0x03); // bDeviceClass
        assert_eq!(desc[5], 0x01); // bDeviceSubClass
        assert_eq!(desc[6], 0x02); // bDeviceProtocol
        assert_eq!(desc[7], 64); // bMaxPacketSize0
        assert_eq!(desc[8], 0x34); // idVendor low
        assert_eq!(desc[9], 0x12); // idVendor high
        assert_eq!(desc[10], 0x78); // idProduct low
        assert_eq!(desc[11], 0x56); // idProduct high
        assert_eq!(desc[17], 1); // bNumConfigurations
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_build_device_descriptor_super_speed() {
        let dev = extender_protocol::UsbDevice {
            path: extender_protocol::UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
            busid: extender_protocol::UsbDevice::busid_from_str("1-1").unwrap(),
            busnum: 1,
            devnum: 1,
            speed: 5, // super-speed
            id_vendor: 0xAAAA,
            id_product: 0xBBBB,
            bcd_device: 0x0200,
            device_class: 0x08,
            device_subclass: 0x06,
            device_protocol: 0x50,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 0,
            interfaces: vec![],
        };

        let desc = build_device_descriptor(&dev);
        assert_eq!(desc[2], 0x00); // bcdUSB low
        assert_eq!(desc[3], 0x03); // bcdUSB high (USB 3.0)
    }
}
