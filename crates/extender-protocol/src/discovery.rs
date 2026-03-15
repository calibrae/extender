//! Discovery message types: DEVLIST and IMPORT.
//!
//! These messages are used in the initial TCP connection to enumerate
//! exported devices and request attachment.

use bytes::{Buf, BufMut};

use crate::codes::{OpCode, USBIP_VERSION};
use crate::device::{
    decode_device_with_interfaces, encode_device_with_interfaces, BusId, UsbDevice,
    USB_DEVICE_WIRE_SIZE, USB_INTERFACE_WIRE_SIZE,
};
use crate::error::ProtocolError;
use crate::wire::WireFormat;

/// Size of the common discovery message header (version + opcode + status).
pub const OP_HEADER_SIZE: usize = 8;

// ── OP_REQ_DEVLIST ──────────────────────────────────────────────────

/// Request the list of exported USB devices.
///
/// Wire format (8 bytes):
/// - version: u16 (0x0111)
/// - command: u16 (0x8005)
/// - status: u32 (0x00000000, unused)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpReqDevlist;

impl WireFormat for OpReqDevlist {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_u16(USBIP_VERSION);
        buf.put_u16(OpCode::OpReqDevlist as u16);
        buf.put_u32(0); // status (unused)
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < OP_HEADER_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: OP_HEADER_SIZE,
                available: buf.remaining(),
            });
        }
        let version = buf.get_u16();
        if version != USBIP_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let code = buf.get_u16();
        if code != OpCode::OpReqDevlist as u16 {
            return Err(ProtocolError::InvalidOpCode(code));
        }
        let _status = buf.get_u32();
        Ok(OpReqDevlist)
    }

    fn wire_size(&self) -> usize {
        OP_HEADER_SIZE
    }
}

// ── OP_REP_DEVLIST ──────────────────────────────────────────────────

/// Reply with the list of exported USB devices.
///
/// Wire format:
/// - version: u16 (0x0111)
/// - command: u16 (0x0005)
/// - status: u32 (0 = success)
/// - num_devices: u32
/// - For each device:
///   - UsbDevice (312 bytes)
///   - UsbInterface[] (4 bytes each, count = device.num_interfaces)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRepDevlist {
    /// Status code (0 = success).
    pub status: u32,
    /// List of exported devices with their interfaces.
    pub devices: Vec<UsbDevice>,
}

impl WireFormat for OpRepDevlist {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_u16(USBIP_VERSION);
        buf.put_u16(OpCode::OpRepDevlist as u16);
        buf.put_u32(self.status);
        buf.put_u32(self.devices.len() as u32);
        for device in &self.devices {
            encode_device_with_interfaces(device, buf);
        }
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < OP_HEADER_SIZE + 4 {
            return Err(ProtocolError::BufferTooShort {
                needed: OP_HEADER_SIZE + 4,
                available: buf.remaining(),
            });
        }

        let version = buf.get_u16();
        if version != USBIP_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let code = buf.get_u16();
        if code != OpCode::OpRepDevlist as u16 {
            return Err(ProtocolError::InvalidOpCode(code));
        }
        let status = buf.get_u32();
        let num_devices = buf.get_u32() as usize;

        let mut devices = Vec::with_capacity(num_devices);
        for _ in 0..num_devices {
            devices.push(decode_device_with_interfaces(buf)?);
        }

        Ok(OpRepDevlist { status, devices })
    }

    fn wire_size(&self) -> usize {
        OP_HEADER_SIZE
            + 4 // num_devices
            + self.devices.iter().map(|d| {
                USB_DEVICE_WIRE_SIZE + d.interfaces.len() * USB_INTERFACE_WIRE_SIZE
            }).sum::<usize>()
    }
}

// ── OP_REQ_IMPORT ───────────────────────────────────────────────────

/// Request to import (attach) a specific device by bus ID.
///
/// Wire format (40 bytes):
/// - version: u16 (0x0111)
/// - command: u16 (0x8003)
/// - status: u32 (0x00000000, unused)
/// - busid: 32 bytes (null-padded)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpReqImport {
    /// The bus ID of the device to import.
    pub busid: BusId,
}

/// Total wire size of OP_REQ_IMPORT.
pub const OP_REQ_IMPORT_SIZE: usize = 40;

impl WireFormat for OpReqImport {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_u16(USBIP_VERSION);
        buf.put_u16(OpCode::OpReqImport as u16);
        buf.put_u32(0); // status (unused)
        buf.put_slice(&self.busid);
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < OP_REQ_IMPORT_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: OP_REQ_IMPORT_SIZE,
                available: buf.remaining(),
            });
        }

        let version = buf.get_u16();
        if version != USBIP_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let code = buf.get_u16();
        if code != OpCode::OpReqImport as u16 {
            return Err(ProtocolError::InvalidOpCode(code));
        }
        let _status = buf.get_u32();

        let mut busid = [0u8; 32];
        buf.copy_to_slice(&mut busid);

        Ok(OpReqImport { busid })
    }

    fn wire_size(&self) -> usize {
        OP_REQ_IMPORT_SIZE
    }
}

// ── OP_REP_IMPORT ───────────────────────────────────────────────────

/// Reply to an import request.
///
/// Wire format:
/// - version: u16 (0x0111)
/// - command: u16 (0x0003)
/// - status: u32 (0 = success, non-zero = error)
/// - If status == 0: device descriptor (312 bytes, no interfaces in import reply)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRepImport {
    /// Status code (0 = success, non-zero = error).
    pub status: u32,
    /// The device descriptor, present only on success.
    pub device: Option<UsbDevice>,
}

impl WireFormat for OpRepImport {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_u16(USBIP_VERSION);
        buf.put_u16(OpCode::OpRepImport as u16);
        buf.put_u32(self.status);
        if let Some(ref device) = self.device {
            device.encode(buf);
        }
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < OP_HEADER_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: OP_HEADER_SIZE,
                available: buf.remaining(),
            });
        }

        let version = buf.get_u16();
        if version != USBIP_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let code = buf.get_u16();
        if code != OpCode::OpRepImport as u16 {
            return Err(ProtocolError::InvalidOpCode(code));
        }
        let status = buf.get_u32();

        let device = if status == 0 {
            Some(UsbDevice::decode(buf)?)
        } else {
            None
        };

        Ok(OpRepImport { status, device })
    }

    fn wire_size(&self) -> usize {
        OP_HEADER_SIZE
            + if self.device.is_some() {
                USB_DEVICE_WIRE_SIZE
            } else {
                0
            }
    }
}

/// Enum over all discovery-phase messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpMessage {
    ReqDevlist(OpReqDevlist),
    RepDevlist(OpRepDevlist),
    ReqImport(OpReqImport),
    RepImport(Box<OpRepImport>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{UsbDevice, UsbInterface};

    #[test]
    fn test_req_devlist_encode_size() {
        let msg = OpReqDevlist;
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 8);
    }

    #[test]
    fn test_req_devlist_encode_bytes() {
        let msg = OpReqDevlist;
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        // version = 0x0111
        assert_eq!(buf[0], 0x01);
        assert_eq!(buf[1], 0x11);
        // command = 0x8005
        assert_eq!(buf[2], 0x80);
        assert_eq!(buf[3], 0x05);
        // status = 0x00000000
        assert_eq!(&buf[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn test_req_devlist_roundtrip() {
        let msg = OpReqDevlist;
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = OpReqDevlist::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_rep_devlist_empty() {
        let msg = OpRepDevlist {
            status: 0,
            devices: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        // 8 (header) + 4 (num_devices) = 12
        assert_eq!(buf.len(), 12);

        let mut cursor = &buf[..];
        let decoded = OpRepDevlist::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    fn make_test_device() -> UsbDevice {
        UsbDevice {
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
            num_interfaces: 1,
            interfaces: vec![UsbInterface {
                interface_class: 0x03,
                interface_subclass: 0x01,
                interface_protocol: 0x02,
                padding: 0,
            }],
        }
    }

    #[test]
    fn test_rep_devlist_with_devices() {
        let msg = OpRepDevlist {
            status: 0,
            devices: vec![make_test_device()],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);

        let mut cursor = &buf[..];
        let decoded = OpRepDevlist::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_req_import_size() {
        let busid = UsbDevice::busid_from_str("1-4.2").unwrap();
        let msg = OpReqImport { busid };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 40);
    }

    #[test]
    fn test_req_import_roundtrip() {
        let busid = UsbDevice::busid_from_str("1-4.2").unwrap();
        let msg = OpReqImport { busid };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = OpReqImport::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_rep_import_success() {
        let device = UsbDevice {
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
            num_interfaces: 0,
            interfaces: vec![],
        };
        let msg = OpRepImport {
            status: 0,
            device: Some(device),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), OP_HEADER_SIZE + USB_DEVICE_WIRE_SIZE);

        let mut cursor = &buf[..];
        let decoded = OpRepImport::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_rep_import_error() {
        let msg = OpRepImport {
            status: 1,
            device: None,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), OP_HEADER_SIZE);

        let mut cursor = &buf[..];
        let decoded = OpRepImport::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_wrong_version_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x02, 0x00]); // wrong version
        buf.extend_from_slice(&[0x80, 0x05]); // OpReqDevlist
        buf.extend_from_slice(&[0, 0, 0, 0]); // status

        let mut cursor = &buf[..];
        let result = OpReqDevlist::decode(&mut cursor);
        assert!(matches!(
            result,
            Err(ProtocolError::UnsupportedVersion(0x0200))
        ));
    }

    #[test]
    fn test_wrong_opcode_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x01, 0x11]); // correct version
        buf.extend_from_slice(&[0x00, 0x05]); // OpRepDevlist, but we're decoding OpReqDevlist
        buf.extend_from_slice(&[0, 0, 0, 0]); // status

        let mut cursor = &buf[..];
        let result = OpReqDevlist::decode(&mut cursor);
        assert!(matches!(result, Err(ProtocolError::InvalidOpCode(0x0005))));
    }

    #[test]
    fn test_buffer_too_short() {
        let buf: &[u8] = &[0x01, 0x11]; // only 2 bytes
        let mut cursor = buf;
        let result = OpReqDevlist::decode(&mut cursor);
        assert!(matches!(
            result,
            Err(ProtocolError::BufferTooShort {
                needed: 8,
                available: 2
            })
        ));
    }
}
