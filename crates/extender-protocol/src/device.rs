//! USB device and interface descriptor types.
//!
//! These structures match the device descriptor format used in
//! OP_REP_DEVLIST and OP_REP_IMPORT messages.

use bytes::{Buf, BufMut};

use crate::error::ProtocolError;
use crate::wire::WireFormat;

/// Size of the device path field in bytes.
pub const DEVICE_PATH_SIZE: usize = 256;
/// Size of the bus ID field in bytes.
pub const BUSID_SIZE: usize = 32;

/// A 32-byte null-terminated bus ID (e.g., "1-4.2").
pub type BusId = [u8; BUSID_SIZE];

/// USB device descriptor as transmitted on the wire.
///
/// Layout (312 bytes without interfaces):
/// - path: 256 bytes (null-terminated string)
/// - busid: 32 bytes (null-terminated string)
/// - busnum: 4 bytes
/// - devnum: 4 bytes
/// - speed: 4 bytes
/// - idVendor: 2 bytes
/// - idProduct: 2 bytes
/// - bcdDevice: 2 bytes
/// - bDeviceClass: 1 byte
/// - bDeviceSubClass: 1 byte
/// - bDeviceProtocol: 1 byte
/// - bConfigurationValue: 1 byte
/// - bNumConfigurations: 1 byte
/// - bNumInterfaces: 1 byte
///
/// Total fixed: 256 + 32 + 4 + 4 + 4 + 2 + 2 + 2 + 1 + 1 + 1 + 1 + 1 + 1 = 312 bytes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    /// Sysfs device path (e.g., "/sys/devices/pci0000:00/...").
    pub path: [u8; DEVICE_PATH_SIZE],
    /// Bus ID string (e.g., "1-4.2").
    pub busid: BusId,
    /// Bus number.
    pub busnum: u32,
    /// Device number.
    pub devnum: u32,
    /// USB speed (1=low, 2=full, 3=high, 5=super).
    pub speed: u32,
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
    /// Current configuration value.
    pub configuration_value: u8,
    /// Number of configurations.
    pub num_configurations: u8,
    /// Number of interfaces.
    pub num_interfaces: u8,
    /// Interface descriptors for this device.
    pub interfaces: Vec<UsbInterface>,
}

/// Size of UsbDevice on the wire (fixed part, without interfaces).
pub const USB_DEVICE_WIRE_SIZE: usize = 312;

impl UsbDevice {
    /// Get the bus ID as a string, trimming null bytes.
    pub fn busid_string(&self) -> &str {
        let end = self
            .busid
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(BUSID_SIZE);
        std::str::from_utf8(&self.busid[..end]).unwrap_or("")
    }

    /// Get the device path as a string, trimming null bytes.
    pub fn path_string(&self) -> &str {
        let end = self
            .path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(DEVICE_PATH_SIZE);
        std::str::from_utf8(&self.path[..end]).unwrap_or("")
    }

    /// Create a BusId from a string, null-padding to 32 bytes.
    pub fn busid_from_str(s: &str) -> Result<BusId, ProtocolError> {
        if s.len() >= BUSID_SIZE {
            return Err(ProtocolError::InvalidBusId(format!(
                "bus ID too long: {} bytes (max {})",
                s.len(),
                BUSID_SIZE - 1
            )));
        }
        if !s.bytes().all(|b| b.is_ascii()) {
            return Err(ProtocolError::InvalidBusId(
                "bus ID must be ASCII".to_string(),
            ));
        }
        let mut busid = [0u8; BUSID_SIZE];
        busid[..s.len()].copy_from_slice(s.as_bytes());
        Ok(busid)
    }

    /// Create a device path from a string, null-padding to 256 bytes.
    pub fn path_from_str(s: &str) -> [u8; DEVICE_PATH_SIZE] {
        let mut path = [0u8; DEVICE_PATH_SIZE];
        let len = s.len().min(DEVICE_PATH_SIZE - 1);
        path[..len].copy_from_slice(&s.as_bytes()[..len]);
        path
    }
}

impl WireFormat for UsbDevice {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_slice(&self.path);
        buf.put_slice(&self.busid);
        buf.put_u32(self.busnum);
        buf.put_u32(self.devnum);
        buf.put_u32(self.speed);
        buf.put_u16(self.id_vendor);
        buf.put_u16(self.id_product);
        buf.put_u16(self.bcd_device);
        buf.put_u8(self.device_class);
        buf.put_u8(self.device_subclass);
        buf.put_u8(self.device_protocol);
        buf.put_u8(self.configuration_value);
        buf.put_u8(self.num_configurations);
        buf.put_u8(self.num_interfaces);
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < USB_DEVICE_WIRE_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: USB_DEVICE_WIRE_SIZE,
                available: buf.remaining(),
            });
        }

        let mut path = [0u8; DEVICE_PATH_SIZE];
        buf.copy_to_slice(&mut path);

        let mut busid = [0u8; BUSID_SIZE];
        buf.copy_to_slice(&mut busid);

        let busnum = buf.get_u32();
        let devnum = buf.get_u32();
        let speed = buf.get_u32();
        let id_vendor = buf.get_u16();
        let id_product = buf.get_u16();
        let bcd_device = buf.get_u16();
        let device_class = buf.get_u8();
        let device_subclass = buf.get_u8();
        let device_protocol = buf.get_u8();
        let configuration_value = buf.get_u8();
        let num_configurations = buf.get_u8();
        let num_interfaces = buf.get_u8();

        Ok(UsbDevice {
            path,
            busid,
            busnum,
            devnum,
            speed,
            id_vendor,
            id_product,
            bcd_device,
            device_class,
            device_subclass,
            device_protocol,
            configuration_value,
            num_configurations,
            num_interfaces,
            interfaces: Vec::new(), // Interfaces decoded separately by caller
        })
    }

    fn wire_size(&self) -> usize {
        USB_DEVICE_WIRE_SIZE
    }
}

/// USB interface descriptor.
///
/// Layout (4 bytes):
/// - bInterfaceClass: 1 byte
/// - bInterfaceSubClass: 1 byte
/// - bInterfaceProtocol: 1 byte
/// - padding: 1 byte
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbInterface {
    /// Interface class code.
    pub interface_class: u8,
    /// Interface subclass code.
    pub interface_subclass: u8,
    /// Interface protocol code.
    pub interface_protocol: u8,
    /// Padding byte (must be 0).
    pub padding: u8,
}

/// Size of UsbInterface on the wire.
pub const USB_INTERFACE_WIRE_SIZE: usize = 4;

impl WireFormat for UsbInterface {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_u8(self.interface_class);
        buf.put_u8(self.interface_subclass);
        buf.put_u8(self.interface_protocol);
        buf.put_u8(self.padding);
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < USB_INTERFACE_WIRE_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: USB_INTERFACE_WIRE_SIZE,
                available: buf.remaining(),
            });
        }

        Ok(UsbInterface {
            interface_class: buf.get_u8(),
            interface_subclass: buf.get_u8(),
            interface_protocol: buf.get_u8(),
            padding: buf.get_u8(),
        })
    }

    fn wire_size(&self) -> usize {
        USB_INTERFACE_WIRE_SIZE
    }
}

/// Decode a UsbDevice along with its interfaces.
///
/// This reads the fixed device descriptor and then reads `num_interfaces`
/// interface descriptors that follow it.
pub fn decode_device_with_interfaces(buf: &mut impl Buf) -> Result<UsbDevice, ProtocolError> {
    let mut device = UsbDevice::decode(buf)?;
    let num_ifaces = device.num_interfaces as usize;

    let needed = num_ifaces * USB_INTERFACE_WIRE_SIZE;
    if buf.remaining() < needed {
        return Err(ProtocolError::BufferTooShort {
            needed,
            available: buf.remaining(),
        });
    }

    device.interfaces.reserve(num_ifaces);
    for _ in 0..num_ifaces {
        device.interfaces.push(UsbInterface::decode(buf)?);
    }

    Ok(device)
}

/// Encode a UsbDevice along with its interfaces.
pub fn encode_device_with_interfaces(device: &UsbDevice, buf: &mut impl BufMut) {
    device.encode(buf);
    for iface in &device.interfaces {
        iface.encode(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_device() -> UsbDevice {
        UsbDevice {
            path: UsbDevice::path_from_str("/sys/devices/pci0000:00/usb1/1-1"),
            busid: UsbDevice::busid_from_str("1-1").unwrap(),
            busnum: 1,
            devnum: 2,
            speed: 3, // high speed
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
        }
    }

    #[test]
    fn test_busid_string() {
        let device = make_test_device();
        assert_eq!(device.busid_string(), "1-1");
    }

    #[test]
    fn test_path_string() {
        let device = make_test_device();
        assert_eq!(device.path_string(), "/sys/devices/pci0000:00/usb1/1-1");
    }

    #[test]
    fn test_busid_from_str_valid() {
        let busid = UsbDevice::busid_from_str("1-4.2").unwrap();
        assert_eq!(&busid[..5], b"1-4.2");
        assert_eq!(busid[5], 0);
    }

    #[test]
    fn test_busid_from_str_too_long() {
        let long = "a".repeat(32);
        assert!(UsbDevice::busid_from_str(&long).is_err());
    }

    #[test]
    fn test_device_roundtrip() {
        let device = make_test_device();
        let mut buf = Vec::new();
        device.encode(&mut buf);
        assert_eq!(buf.len(), USB_DEVICE_WIRE_SIZE);

        let mut cursor = &buf[..];
        let decoded = UsbDevice::decode(&mut cursor).unwrap();
        // Interfaces are not included in the basic decode
        assert_eq!(decoded.busnum, device.busnum);
        assert_eq!(decoded.id_vendor, device.id_vendor);
        assert_eq!(decoded.id_product, device.id_product);
    }

    #[test]
    fn test_device_with_interfaces_roundtrip() {
        let device = make_test_device();
        let mut buf = Vec::new();
        encode_device_with_interfaces(&device, &mut buf);
        assert_eq!(
            buf.len(),
            USB_DEVICE_WIRE_SIZE + 2 * USB_INTERFACE_WIRE_SIZE
        );

        let mut cursor = &buf[..];
        let decoded = decode_device_with_interfaces(&mut cursor).unwrap();
        assert_eq!(decoded, device);
    }

    #[test]
    fn test_interface_roundtrip() {
        let iface = UsbInterface {
            interface_class: 0x03,
            interface_subclass: 0x01,
            interface_protocol: 0x02,
            padding: 0,
        };
        let mut buf = Vec::new();
        iface.encode(&mut buf);
        assert_eq!(buf.len(), USB_INTERFACE_WIRE_SIZE);

        let mut cursor = &buf[..];
        let decoded = UsbInterface::decode(&mut cursor).unwrap();
        assert_eq!(decoded, iface);
    }
}
