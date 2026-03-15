//! VHCI (Virtual Host Controller Interface) sysfs driver for Linux.
//!
//! On Linux, the `vhci_hcd` kernel module exposes a sysfs interface
//! at `/sys/devices/platform/vhci_hcd.0/` for attaching and detaching
//! remote USB devices.
//!
//! This module is only compiled on Linux targets.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::ClientError;
use crate::types::{PortStatus, VhciPort};

/// Default sysfs path for the vhci_hcd controller.
const VHCI_SYSFS_PATH: &str = "/sys/devices/platform/vhci_hcd.0";

/// Trait abstracting the VHCI interface for testing and platform flexibility.
pub trait VirtualHci: Send + Sync {
    /// Attach a device to a VHCI port.
    ///
    /// # Arguments
    /// * `port` - The port number to attach to.
    /// * `fd` - The raw socket file descriptor for the USB/IP connection.
    /// * `devid` - The device ID (busnum << 16 | devnum).
    /// * `speed` - The USB speed of the device.
    fn attach(&self, port: u32, fd: i32, devid: u32, speed: u32) -> Result<(), ClientError>;

    /// Detach a device from a VHCI port.
    fn detach(&self, port: u32) -> Result<(), ClientError>;

    /// List all VHCI ports and their current status.
    fn list_ports(&self) -> Result<Vec<VhciPort>, ClientError>;

    /// Find a free port matching the given USB speed.
    ///
    /// High-speed (speed <= 3) devices use "hs" hub ports.
    /// Super-speed (speed >= 5) devices use "ss" hub ports.
    fn find_free_port(&self, speed: u32) -> Result<u32, ClientError>;
}

/// Linux VHCI driver that interacts with vhci_hcd via sysfs.
pub struct VhciDriver {
    /// Path to the vhci_hcd sysfs directory.
    sysfs_path: PathBuf,
}

impl VhciDriver {
    /// Create a new VhciDriver, verifying that the sysfs path exists.
    pub fn new() -> Result<Self, ClientError> {
        Self::with_sysfs_path(VHCI_SYSFS_PATH)
    }

    /// Create a VhciDriver with a custom sysfs path (useful for testing).
    pub fn with_sysfs_path(path: impl Into<PathBuf>) -> Result<Self, ClientError> {
        let sysfs_path = path.into();
        if !sysfs_path.exists() {
            return Err(ClientError::VhciNotAvailable {
                reason: format!("sysfs path {} does not exist", sysfs_path.display()),
            });
        }
        Ok(VhciDriver { sysfs_path })
    }

    /// Path to the status file.
    fn status_path(&self) -> PathBuf {
        self.sysfs_path.join("status")
    }

    /// Path to the attach file.
    fn attach_path(&self) -> PathBuf {
        self.sysfs_path.join("attach")
    }

    /// Path to the detach file.
    fn detach_path(&self) -> PathBuf {
        self.sysfs_path.join("detach")
    }
}

impl VirtualHci for VhciDriver {
    fn attach(&self, port: u32, fd: i32, devid: u32, speed: u32) -> Result<(), ClientError> {
        let attach_path = self.attach_path();
        let data = format!("{port} {fd} {devid} {speed}");
        tracing::debug!(path = %attach_path.display(), data = %data, "writing to vhci attach");
        fs::write(&attach_path, &data).map_err(|e| ClientError::Io(e))
    }

    fn detach(&self, port: u32) -> Result<(), ClientError> {
        let detach_path = self.detach_path();
        let data = format!("{port}");
        tracing::debug!(path = %detach_path.display(), data = %data, "writing to vhci detach");
        fs::write(&detach_path, &data).map_err(|e| ClientError::Io(e))
    }

    fn list_ports(&self) -> Result<Vec<VhciPort>, ClientError> {
        let status_path = self.status_path();
        let content = fs::read_to_string(&status_path).map_err(ClientError::Io)?;
        parse_vhci_status(&content)
    }

    fn find_free_port(&self, speed: u32) -> Result<u32, ClientError> {
        let ports = self.list_ports()?;
        find_free_port_in_list(&ports, speed)
    }
}

/// Parse the content of a vhci_hcd status file.
///
/// The format is:
/// ```text
/// hub port sta spd dev      sockfd local_busid
/// hs  0000 004 000 00000000 000000 0-0
/// hs  0001 004 000 00000000 000000 0-0
/// ...
/// ss  0008 004 000 00000000 000000 0-0
/// ```
///
/// There may be multiple header lines (one per hub section).
pub fn parse_vhci_status(content: &str) -> Result<Vec<VhciPort>, ClientError> {
    let mut ports = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        // Skip header lines
        if line.is_empty() || line.starts_with("hub") {
            continue;
        }

        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 7 {
            // Skip lines that don't have enough fields (could be partial)
            continue;
        }

        let hub = fields[0].to_owned();
        let port = parse_hex_u32(fields[1]).map_err(|e| ClientError::VhciParseError {
            reason: format!("bad port number '{}': {e}", fields[1]),
        })?;
        let status_val = parse_hex_u32(fields[2]).map_err(|e| ClientError::VhciParseError {
            reason: format!("bad status '{}': {e}", fields[2]),
        })?;
        let speed = parse_hex_u32(fields[3]).map_err(|e| ClientError::VhciParseError {
            reason: format!("bad speed '{}': {e}", fields[3]),
        })?;
        let devid = parse_hex_u32(fields[4]).map_err(|e| ClientError::VhciParseError {
            reason: format!("bad devid '{}': {e}", fields[4]),
        })?;
        let sockfd = parse_hex_u32(fields[5]).map_err(|e| ClientError::VhciParseError {
            reason: format!("bad sockfd '{}': {e}", fields[5]),
        })?;
        let local_busid = fields[6].to_owned();

        ports.push(VhciPort {
            hub,
            port,
            status: PortStatus::from_raw(status_val),
            speed,
            devid,
            sockfd,
            local_busid,
        });
    }

    Ok(ports)
}

/// Find a free port in the given list that matches the requested USB speed.
///
/// High-speed devices (speed <= 3) go on "hs" hub ports.
/// Super-speed devices (speed >= 5) go on "ss" hub ports.
pub fn find_free_port_in_list(ports: &[VhciPort], speed: u32) -> Result<u32, ClientError> {
    let target_hub = if speed >= 5 { "ss" } else { "hs" };

    ports
        .iter()
        .find(|p| p.hub == target_hub && p.status.is_free())
        .map(|p| p.port)
        .ok_or(ClientError::NoFreePort { speed })
}

/// Parse a hexadecimal string (without 0x prefix) into u32.
fn parse_hex_u32(s: &str) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(s, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_STATUS: &str = "\
hub port sta spd dev      sockfd local_busid
hs  0000 004 000 00000000 000000 0-0
hs  0001 004 000 00000000 000000 0-0
hs  0002 006 002 00010002 000003 1-1
hs  0003 004 000 00000000 000000 0-0
hub port sta spd dev      sockfd local_busid
ss  0004 004 000 00000000 000000 0-0
ss  0005 004 000 00000000 000000 0-0
ss  0006 006 005 00020003 000004 2-1
ss  0007 004 000 00000000 000000 0-0
";

    #[test]
    fn test_parse_vhci_status() {
        let ports = parse_vhci_status(SAMPLE_STATUS).unwrap();
        assert_eq!(ports.len(), 8);

        // First port: hs, free
        assert_eq!(ports[0].hub, "hs");
        assert_eq!(ports[0].port, 0);
        assert_eq!(ports[0].status, PortStatus::Free);
        assert_eq!(ports[0].speed, 0);
        assert_eq!(ports[0].devid, 0);
        assert_eq!(ports[0].local_busid, "0-0");

        // Third port: hs, in use
        assert_eq!(ports[2].hub, "hs");
        assert_eq!(ports[2].port, 2);
        assert_eq!(ports[2].status, PortStatus::InUse);
        assert_eq!(ports[2].speed, 2);
        assert_eq!(ports[2].devid, 0x00010002);
        assert_eq!(ports[2].sockfd, 3);
        assert_eq!(ports[2].local_busid, "1-1");

        // Seventh port: ss, in use
        assert_eq!(ports[6].hub, "ss");
        assert_eq!(ports[6].port, 6);
        assert_eq!(ports[6].status, PortStatus::InUse);
        assert_eq!(ports[6].speed, 5);
        assert_eq!(ports[6].devid, 0x00020003);
    }

    #[test]
    fn test_parse_empty_status() {
        let ports = parse_vhci_status("hub port sta spd dev      sockfd local_busid\n").unwrap();
        assert!(ports.is_empty());
    }

    #[test]
    fn test_find_free_port_high_speed() {
        let ports = parse_vhci_status(SAMPLE_STATUS).unwrap();
        // Speed 2 (full) should find hs port 0 (first free hs port)
        let port = find_free_port_in_list(&ports, 2).unwrap();
        assert_eq!(port, 0);
    }

    #[test]
    fn test_find_free_port_super_speed() {
        let ports = parse_vhci_status(SAMPLE_STATUS).unwrap();
        // Speed 5 (super) should find ss port 4 (first free ss port)
        let port = find_free_port_in_list(&ports, 5).unwrap();
        assert_eq!(port, 4);
    }

    #[test]
    fn test_find_free_port_none_available() {
        let status = "\
hub port sta spd dev      sockfd local_busid
hs  0000 006 002 00010001 000001 1-1
hs  0001 006 003 00010002 000002 1-2
";
        let ports = parse_vhci_status(status).unwrap();
        // All hs ports in use, requesting high-speed
        let result = find_free_port_in_list(&ports, 3);
        assert!(matches!(result, Err(ClientError::NoFreePort { speed: 3 })));
    }

    #[test]
    fn test_parse_status_with_trailing_whitespace() {
        let status = "hub port sta spd dev      sockfd local_busid\n  hs  0000 004 000 00000000 000000 0-0  \n";
        let ports = parse_vhci_status(status).unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].hub, "hs");
        assert!(ports[0].status.is_free());
    }
}
