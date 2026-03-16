//! USB device enumeration and filtering.
//!
//! Provides [`LocalUsbDevice`] representing a locally connected USB device,
//! [`enumerate_devices`] for discovering all connected devices via rusb/libusb,
//! and [`filter_devices`] for narrowing down the list by class, VID:PID, or bus ID.

use rusb::UsbContext;

use extender_protocol::{UsbDevice, UsbInterface};

use crate::error::ServerError;

/// A locally connected USB device with metadata gathered from rusb.
///
/// This is the server-side representation. It can be converted to the
/// wire-format [`UsbDevice`] for protocol transmission.
#[derive(Debug, Clone)]
pub struct LocalUsbDevice {
    /// USB bus number.
    pub bus_number: u8,
    /// Device address on the bus.
    pub device_address: u8,
    /// Vendor ID.
    pub vendor_id: u16,
    /// Product ID.
    pub product_id: u16,
    /// Manufacturer string (if readable).
    pub manufacturer: Option<String>,
    /// Product string (if readable).
    pub product: Option<String>,
    /// USB device class code.
    pub device_class: u8,
    /// USB device subclass code.
    pub device_subclass: u8,
    /// USB device protocol code.
    pub device_protocol: u8,
    /// BCD device release number.
    pub bcd_device: u16,
    /// Bus ID string in Linux kernel format (e.g., "1-2.3").
    pub bus_id: String,
    /// USB speed (1=low, 2=full, 3=high, 5=super).
    pub speed: u32,
    /// Number of configurations.
    pub num_configurations: u8,
    /// Interface descriptors for the active configuration.
    pub interfaces: Vec<LocalUsbInterface>,
    /// Port numbers forming the device's topology path.
    pub port_numbers: Vec<u8>,
}

/// A USB interface descriptor from local enumeration.
#[derive(Debug, Clone)]
pub struct LocalUsbInterface {
    /// Interface number.
    pub interface_number: u8,
    /// Interface class code.
    pub interface_class: u8,
    /// Interface subclass code.
    pub interface_subclass: u8,
    /// Interface protocol code.
    pub interface_protocol: u8,
}

impl LocalUsbDevice {
    /// Convert to the wire-format [`UsbDevice`] for protocol transmission.
    pub fn to_protocol_device(&self) -> Result<UsbDevice, ServerError> {
        // Format path as a synthetic sysfs path.
        let path_str = format!("/sys/devices/usb{}/{}", self.bus_number, self.bus_id);
        let path = UsbDevice::path_from_str(&path_str);
        let busid = UsbDevice::busid_from_str(&self.bus_id)?;

        let interfaces: Vec<UsbInterface> = self
            .interfaces
            .iter()
            .map(|iface| UsbInterface {
                interface_class: iface.interface_class,
                interface_subclass: iface.interface_subclass,
                interface_protocol: iface.interface_protocol,
                padding: 0,
            })
            .collect();

        Ok(UsbDevice {
            path,
            busid,
            busnum: self.bus_number as u32,
            devnum: self.device_address as u32,
            speed: self.speed,
            id_vendor: self.vendor_id,
            id_product: self.product_id,
            bcd_device: self.bcd_device,
            device_class: self.device_class,
            device_subclass: self.device_subclass,
            device_protocol: self.device_protocol,
            configuration_value: 0,
            num_configurations: self.num_configurations,
            num_interfaces: interfaces.len() as u8,
            interfaces,
        })
    }
}

/// Format a bus ID from bus number and port numbers.
///
/// Follows the Linux kernel convention: "busnum-port1.port2.port3..."
/// For root hub devices (no port numbers), returns "busnum-0".
fn format_bus_id(bus_number: u8, port_numbers: &[u8]) -> String {
    if port_numbers.is_empty() {
        // Root hub or device with no port info.
        return format!("{}-0", bus_number);
    }
    let ports: Vec<String> = port_numbers.iter().map(|p| p.to_string()).collect();
    let first = &ports[0];
    if ports.len() == 1 {
        format!("{}-{}", bus_number, first)
    } else {
        format!("{}-{}.{}", bus_number, first, ports[1..].join("."))
    }
}

/// Map rusb speed to USB/IP speed value.
///
/// USB/IP protocol uses: 1=low, 2=full, 3=high, 5=super, 6=super+
fn map_speed(speed: rusb::Speed) -> u32 {
    match speed {
        rusb::Speed::Low => 1,
        rusb::Speed::Full => 2,
        rusb::Speed::High => 3,
        rusb::Speed::Super => 5,
        _ => 0, // unknown
    }
}

/// Enumerate all locally connected USB devices.
///
/// Returns a list of [`LocalUsbDevice`] structs. Devices for which
/// descriptor access is denied will still be returned with `None` for
/// string descriptors. If libusb initialization fails, an error is returned.
pub fn enumerate_devices() -> Result<Vec<LocalUsbDevice>, ServerError> {
    let context = rusb::Context::new().map_err(ServerError::UsbContextInit)?;
    let device_list = context.devices().map_err(ServerError::Enumeration)?;

    let mut result = Vec::new();

    for device in device_list.iter() {
        let desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    bus = device.bus_number(),
                    addr = device.address(),
                    "skipping device: failed to read descriptor: {}",
                    e
                );
                continue;
            }
        };

        let bus_number = device.bus_number();
        let device_address = device.address();
        let port_numbers = device.port_numbers().unwrap_or_default();
        let bus_id = format_bus_id(bus_number, &port_numbers);
        let speed = map_speed(device.speed());

        // Try to read string descriptors by opening the device.
        // This may fail due to permissions, which is fine.
        let (manufacturer, product) = match device.open() {
            Ok(handle) => {
                let timeout = std::time::Duration::from_millis(500);
                let lang = handle
                    .read_languages(timeout)
                    .ok()
                    .and_then(|langs| langs.into_iter().next());

                let mfr =
                    lang.and_then(|l| handle.read_manufacturer_string(l, &desc, timeout).ok());
                let prod = lang.and_then(|l| handle.read_product_string(l, &desc, timeout).ok());
                (mfr, prod)
            }
            Err(_) => (None, None),
        };

        // Read interface descriptors from the active (first) configuration.
        let interfaces = match device.active_config_descriptor() {
            Ok(config) => config
                .interfaces()
                .flat_map(|iface| {
                    iface.descriptors().next().map(|d| LocalUsbInterface {
                        interface_number: d.interface_number(),
                        interface_class: d.class_code(),
                        interface_subclass: d.sub_class_code(),
                        interface_protocol: d.protocol_code(),
                    })
                })
                .collect(),
            Err(_) => {
                // Fall back to reading config descriptor 0.
                match device.config_descriptor(0) {
                    Ok(config) => config
                        .interfaces()
                        .flat_map(|iface| {
                            iface.descriptors().next().map(|d| LocalUsbInterface {
                                interface_number: d.interface_number(),
                                interface_class: d.class_code(),
                                interface_subclass: d.sub_class_code(),
                                interface_protocol: d.protocol_code(),
                            })
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                }
            }
        };

        result.push(LocalUsbDevice {
            bus_number,
            device_address,
            vendor_id: desc.vendor_id(),
            product_id: desc.product_id(),
            manufacturer,
            product,
            device_class: desc.class_code(),
            device_subclass: desc.sub_class_code(),
            device_protocol: desc.protocol_code(),
            bcd_device: {
                let v = desc.device_version();
                ((v.major() as u16) << 8) | ((v.minor() as u16) << 4) | (v.sub_minor() as u16)
            },
            bus_id,
            speed,
            num_configurations: desc.num_configurations(),
            interfaces,
            port_numbers,
        });
    }

    Ok(result)
}

/// Filter criteria for USB devices.
#[derive(Debug, Clone, Default)]
pub struct DeviceFilter {
    /// Filter by USB device class code.
    pub device_class: Option<u8>,
    /// Filter by vendor ID and product ID.
    pub vid_pid: Option<(u16, u16)>,
    /// Filter by bus ID glob pattern (e.g., "1-4.*").
    pub bus_id_pattern: Option<String>,
}

/// Filter a list of devices according to the given criteria.
///
/// All specified criteria must match (logical AND). Returns references
/// to devices that match all non-None filter fields.
pub fn filter_devices<'a>(
    devices: &'a [LocalUsbDevice],
    filter: &DeviceFilter,
) -> Vec<&'a LocalUsbDevice> {
    devices
        .iter()
        .filter(|dev| {
            if let Some(class) = filter.device_class {
                // Check device-level class, or any interface-level class.
                let device_matches = dev.device_class == class;
                let interface_matches = dev
                    .interfaces
                    .iter()
                    .any(|iface| iface.interface_class == class);
                if !device_matches && !interface_matches {
                    return false;
                }
            }

            if let Some((vid, pid)) = filter.vid_pid {
                if dev.vendor_id != vid || dev.product_id != pid {
                    return false;
                }
            }

            if let Some(ref pattern) = filter.bus_id_pattern {
                if !glob_match(pattern, &dev.bus_id) {
                    return false;
                }
            }

            true
        })
        .collect()
}

/// Simple glob pattern matching supporting `*` (any chars) and `?` (single char).
///
/// This is intentionally minimal -- it handles the bus ID patterns like
/// "1-4.*" that the CLI will produce.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    glob_match_inner(&pat, &txt)
}

fn glob_match_inner(pattern: &[char], text: &[char]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pi = None;
    let mut star_ti = 0;

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(spi) = star_pi {
            pi = spi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }

    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_device(
        bus_id: &str,
        vid: u16,
        pid: u16,
        device_class: u8,
        interfaces: Vec<LocalUsbInterface>,
    ) -> LocalUsbDevice {
        LocalUsbDevice {
            bus_number: 1,
            device_address: 1,
            vendor_id: vid,
            product_id: pid,
            manufacturer: Some("Test Mfr".to_string()),
            product: Some("Test Product".to_string()),
            device_class,
            device_subclass: 0,
            device_protocol: 0,
            bcd_device: 0x0100,
            bus_id: bus_id.to_string(),
            speed: 3,
            num_configurations: 1,
            interfaces,
            port_numbers: vec![1],
        }
    }

    fn make_iface(class: u8) -> LocalUsbInterface {
        LocalUsbInterface {
            interface_number: 0,
            interface_class: class,
            interface_subclass: 0,
            interface_protocol: 0,
        }
    }

    // -- glob matching tests --

    #[test]
    fn test_glob_exact() {
        assert!(glob_match("1-4.2", "1-4.2"));
        assert!(!glob_match("1-4.2", "1-4.3"));
    }

    #[test]
    fn test_glob_star() {
        assert!(glob_match("1-4.*", "1-4.2"));
        assert!(glob_match("1-4.*", "1-4.2.3"));
        assert!(!glob_match("1-4.*", "1-5.2"));
    }

    #[test]
    fn test_glob_question() {
        assert!(glob_match("1-?", "1-4"));
        assert!(!glob_match("1-?", "1-42"));
    }

    #[test]
    fn test_glob_star_prefix() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*-4", "1-4"));
    }

    // -- filter tests --

    #[test]
    fn test_filter_by_class_device_level() {
        let devices = vec![
            make_device("1-1", 0x1234, 0x5678, 0x03, vec![]),
            make_device("1-2", 0x1234, 0x5679, 0x08, vec![]),
        ];
        let filter = DeviceFilter {
            device_class: Some(0x03),
            ..Default::default()
        };
        let result = filter_devices(&devices, &filter);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bus_id, "1-1");
    }

    #[test]
    fn test_filter_by_class_interface_level() {
        // Device class 0x00 (defined at interface level), but interface is HID (0x03)
        let devices = vec![make_device(
            "1-1",
            0x1234,
            0x5678,
            0x00,
            vec![make_iface(0x03)],
        )];
        let filter = DeviceFilter {
            device_class: Some(0x03),
            ..Default::default()
        };
        let result = filter_devices(&devices, &filter);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_filter_by_vid_pid() {
        let devices = vec![
            make_device("1-1", 0x1234, 0x5678, 0, vec![]),
            make_device("1-2", 0xAAAA, 0xBBBB, 0, vec![]),
        ];
        let filter = DeviceFilter {
            vid_pid: Some((0x1234, 0x5678)),
            ..Default::default()
        };
        let result = filter_devices(&devices, &filter);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bus_id, "1-1");
    }

    #[test]
    fn test_filter_by_bus_id_pattern() {
        let devices = vec![
            make_device("1-4.1", 0x1234, 0x5678, 0, vec![]),
            make_device("1-4.2", 0x1234, 0x5679, 0, vec![]),
            make_device("2-1", 0x1234, 0x567A, 0, vec![]),
        ];
        let filter = DeviceFilter {
            bus_id_pattern: Some("1-4.*".to_string()),
            ..Default::default()
        };
        let result = filter_devices(&devices, &filter);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_filter_combined() {
        let devices = vec![
            make_device("1-4.1", 0x1234, 0x5678, 0x03, vec![]),
            make_device("1-4.2", 0x1234, 0x5678, 0x08, vec![]),
            make_device("2-1", 0x1234, 0x5678, 0x03, vec![]),
        ];
        let filter = DeviceFilter {
            device_class: Some(0x03),
            bus_id_pattern: Some("1-4.*".to_string()),
            ..Default::default()
        };
        let result = filter_devices(&devices, &filter);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].bus_id, "1-4.1");
    }

    #[test]
    fn test_filter_no_match() {
        let devices = vec![make_device("1-1", 0x1234, 0x5678, 0, vec![])];
        let filter = DeviceFilter {
            vid_pid: Some((0xFFFF, 0xFFFF)),
            ..Default::default()
        };
        let result = filter_devices(&devices, &filter);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_empty_returns_all() {
        let devices = vec![
            make_device("1-1", 0x1234, 0x5678, 0, vec![]),
            make_device("1-2", 0xAAAA, 0xBBBB, 0, vec![]),
        ];
        let filter = DeviceFilter::default();
        let result = filter_devices(&devices, &filter);
        assert_eq!(result.len(), 2);
    }

    // -- bus ID formatting tests --

    #[test]
    fn test_format_bus_id_single_port() {
        assert_eq!(format_bus_id(1, &[4]), "1-4");
    }

    #[test]
    fn test_format_bus_id_multi_port() {
        assert_eq!(format_bus_id(1, &[4, 2]), "1-4.2");
        assert_eq!(format_bus_id(2, &[1, 3, 7]), "2-1.3.7");
    }

    #[test]
    fn test_format_bus_id_no_ports() {
        assert_eq!(format_bus_id(1, &[]), "1-0");
    }

    // -- to_protocol_device tests --

    #[test]
    fn test_to_protocol_device() {
        let dev = make_device("1-4.2", 0x1234, 0x5678, 0x03, vec![make_iface(0x03)]);
        let proto = dev.to_protocol_device().unwrap();
        assert_eq!(proto.busnum, 1);
        assert_eq!(proto.id_vendor, 0x1234);
        assert_eq!(proto.id_product, 0x5678);
        assert_eq!(proto.device_class, 0x03);
        assert_eq!(proto.busid_string(), "1-4.2");
        assert_eq!(proto.num_interfaces, 1);
        assert_eq!(proto.interfaces[0].interface_class, 0x03);
    }

    // -- enumerate_devices integration test (requires actual USB) --

    #[test]
    #[ignore]
    fn test_enumerate_devices_live() {
        let devices = enumerate_devices().expect("enumeration should succeed");
        // Just verify it doesn't panic and returns a list.
        println!("Found {} USB devices:", devices.len());
        for dev in &devices {
            println!(
                "  {} {:04x}:{:04x} {} {}",
                dev.bus_id,
                dev.vendor_id,
                dev.product_id,
                dev.manufacturer.as_deref().unwrap_or("?"),
                dev.product.as_deref().unwrap_or("?"),
            );
        }
    }
}
