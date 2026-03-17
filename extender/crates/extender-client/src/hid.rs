//! USB HID (Human Interface Device) protocol handler.
//!
//! This module implements the USB HID class protocol over USB/IP. It supports
//! reading input reports (keyboard, mouse, gamepad) via interrupt IN transfers,
//! sending output reports (e.g., LED state) via interrupt OUT or SET_REPORT
//! control transfers, and issuing HID-specific control transfers such as
//! GET_DESCRIPTOR (report descriptor), SET_IDLE, and SET_PROTOCOL.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;

use extender_protocol::codec::{read_op_message, read_urb_message, write_op_message};
use extender_protocol::urb::{CmdSubmit, UsbipHeaderBasic};
use extender_protocol::wire::WireFormat;
use extender_protocol::{Command, OpMessage, OpReqImport, UsbDevice};

use crate::error::ClientError;

// ── Constants ────────────────────────────────────────────────────────

/// Default interrupt IN endpoint (e.g., keyboard/mouse input).
const DEFAULT_EP_IN: u8 = 0x81;

/// Default interrupt OUT endpoint (e.g., keyboard LED output).
const DEFAULT_EP_OUT: u8 = 0x02;

/// Connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// Default HID report descriptor max length for initial request.
const DEFAULT_REPORT_DESCRIPTOR_MAX_LEN: u16 = 4096;

/// HID class-specific descriptor type: Report Descriptor.
const HID_DESCRIPTOR_TYPE_REPORT: u8 = 0x22;

/// HID class request: SET_IDLE.
const HID_REQUEST_SET_IDLE: u8 = 0x0A;

/// HID class request: SET_PROTOCOL.
const HID_REQUEST_SET_PROTOCOL: u8 = 0x0B;

/// HID class request: SET_REPORT.
const HID_REQUEST_SET_REPORT: u8 = 0x09;

/// USB request type: device-to-host, class, interface.
const USB_RT_HID_INTERFACE_IN: u8 = 0x81;

/// USB request type: host-to-device, class, interface.
const USB_RT_HID_INTERFACE_OUT: u8 = 0x21;

/// USB standard request: GET_DESCRIPTOR.
const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;

// ── HidDevice ────────────────────────────────────────────────────────

/// A USB HID device accessed over USB/IP.
///
/// Wraps a TCP connection to a USB/IP server and provides HID-level
/// operations: reading input reports, writing output reports, and
/// issuing HID-specific control transfers.
pub struct HidDevice {
    /// Read half of the TCP stream (after OP_REQ_IMPORT).
    reader: OwnedReadHalf,
    /// Write half of the TCP stream.
    writer: OwnedWriteHalf,
    /// Device ID from the IMPORT response.
    devid: u32,
    /// Interrupt IN endpoint (e.g., 0x81).
    ep_in: u8,
    /// Interrupt OUT endpoint (optional, e.g., 0x02).
    ep_out: Option<u8>,
    /// Next sequence number for USB/IP URBs.
    next_seqnum: u32,
    /// Raw HID report descriptor bytes.
    report_descriptor: Vec<u8>,
    /// USB vendor ID.
    vendor_id: u16,
    /// USB product ID.
    product_id: u16,
    /// Interface number (HID devices can have multiple interfaces).
    interface_number: u8,
}

impl HidDevice {
    /// Connect to a USB/IP server and import a HID device.
    ///
    /// Sends OP_REQ_IMPORT for the given `busid` and, on success,
    /// returns a `HidDevice` ready for `initialize()`.
    pub async fn connect(addr: SocketAddr, busid: &str) -> Result<Self, ClientError> {
        let connect_timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

        let stream = timeout(connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| ClientError::ConnectTimeout {
                addr,
                timeout_secs: CONNECT_TIMEOUT_SECS,
            })?
            .map_err(ClientError::Io)?;

        let (mut reader, mut writer) = stream.into_split();

        // Send OP_REQ_IMPORT
        let busid_bytes = UsbDevice::busid_from_str(busid)
            .map_err(|_| ClientError::InvalidBusId(busid.to_string()))?;
        let req = OpMessage::ReqImport(OpReqImport { busid: busid_bytes });
        write_op_message(&mut writer, &req).await?;

        // Read OP_REP_IMPORT
        let reply = read_op_message(&mut reader).await?;
        let (devid, device) = match reply {
            OpMessage::RepImport(rep) => {
                if rep.status != 0 {
                    return Err(ClientError::ImportRejected {
                        busid: busid.to_string(),
                        status: rep.status,
                    });
                }
                let device = rep.device.ok_or(ClientError::ImportMissingDevice)?;
                let devid = (device.busnum << 16) | device.devnum;
                (devid, device)
            }
            _ => {
                return Err(ClientError::Protocol(
                    extender_protocol::ProtocolError::InvalidOpCode(0),
                ));
            }
        };

        Ok(HidDevice {
            reader,
            writer,
            devid,
            ep_in: DEFAULT_EP_IN,
            ep_out: Some(DEFAULT_EP_OUT),
            next_seqnum: 1,
            report_descriptor: Vec::new(),
            vendor_id: device.id_vendor,
            product_id: device.id_product,
            interface_number: 0,
        })
    }

    /// Initialize the HID device.
    ///
    /// Retrieves the HID report descriptor, sets idle rate to zero
    /// (only report on change), and sets the device to report protocol.
    pub async fn initialize(&mut self) -> Result<(), ClientError> {
        // Get the HID report descriptor
        let descriptor = self.get_report_descriptor().await?;
        self.report_descriptor = descriptor;

        // SET_IDLE: duration=0 (indefinite), report_id=0 (all reports)
        self.set_idle(0, 0).await?;

        // SET_PROTOCOL: 1 = report protocol (as opposed to 0 = boot protocol)
        self.set_protocol(1).await?;

        Ok(())
    }

    /// Read the next input report from the device.
    ///
    /// Sends an interrupt IN transfer and blocks until the device has data.
    /// The returned `Vec<u8>` contains the raw report bytes (e.g., 8 bytes
    /// for a keyboard boot report).
    pub async fn read_report(&mut self) -> Result<Vec<u8>, ClientError> {
        // HID input reports come via interrupt IN endpoint.
        // We request a reasonably large buffer; the device will return
        // only the actual report size.
        let data = self.receive_interrupt_in(64).await?;
        Ok(data)
    }

    /// Send an output report to the device.
    ///
    /// If an interrupt OUT endpoint is available, uses that. Otherwise,
    /// falls back to a SET_REPORT control transfer on EP0.
    pub async fn write_report(&mut self, report: &[u8]) -> Result<(), ClientError> {
        if let Some(ep_out) = self.ep_out {
            // Use interrupt OUT endpoint
            self.send_interrupt_out(ep_out, report).await?;
            self.read_ret_submit().await?;
        } else {
            // Fall back to SET_REPORT control transfer
            self.set_report(report).await?;
        }
        Ok(())
    }

    /// Get the cached HID report descriptor.
    pub fn report_descriptor(&self) -> &[u8] {
        &self.report_descriptor
    }

    /// Get the USB vendor ID.
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    /// Get the USB product ID.
    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    /// Get the interface number.
    pub fn interface_number(&self) -> u8 {
        self.interface_number
    }

    // ── HID-specific control transfers ──────────────────────────────

    /// Get the HID report descriptor via GET_DESCRIPTOR control transfer.
    ///
    /// Setup packet: [0x81, 0x06, 0x00, 0x22, iface, 0x00, len_lo, len_hi]
    ///   - bmRequestType = 0x81 (device-to-host, standard, interface)
    ///   - bRequest = 0x06 (GET_DESCRIPTOR)
    ///   - wValue = 0x2200 (HID Report Descriptor, index 0)
    ///   - wIndex = interface number
    ///   - wLength = max descriptor length
    async fn get_report_descriptor(&mut self) -> Result<Vec<u8>, ClientError> {
        let len = DEFAULT_REPORT_DESCRIPTOR_MAX_LEN;
        let setup = build_get_report_descriptor_setup(self.interface_number, len);
        self.control_transfer_in(&setup, len as u32).await
    }

    /// SET_IDLE control transfer.
    ///
    /// Setup packet: [0x21, 0x0A, report_id, duration, iface, 0x00, 0x00, 0x00]
    ///   - bmRequestType = 0x21 (host-to-device, class, interface)
    ///   - bRequest = 0x0A (SET_IDLE)
    ///   - wValue = (duration << 8) | report_id
    ///   - wIndex = interface number
    ///   - wLength = 0
    async fn set_idle(&mut self, duration: u8, report_id: u8) -> Result<(), ClientError> {
        let setup = build_set_idle_setup(self.interface_number, duration, report_id);
        self.control_transfer_out(&setup, &[]).await
    }

    /// SET_PROTOCOL control transfer.
    ///
    /// Setup packet: [0x21, 0x0B, protocol, 0x00, iface, 0x00, 0x00, 0x00]
    ///   - bmRequestType = 0x21 (host-to-device, class, interface)
    ///   - bRequest = 0x0B (SET_PROTOCOL)
    ///   - wValue = protocol (0=boot, 1=report)
    ///   - wIndex = interface number
    ///   - wLength = 0
    async fn set_protocol(&mut self, protocol: u8) -> Result<(), ClientError> {
        let setup = build_set_protocol_setup(self.interface_number, protocol);
        self.control_transfer_out(&setup, &[]).await
    }

    /// SET_REPORT control transfer (fallback for output reports without interrupt OUT).
    ///
    /// Setup packet: [0x21, 0x09, report_id, report_type, iface, 0x00, len_lo, len_hi]
    ///   - bmRequestType = 0x21 (host-to-device, class, interface)
    ///   - bRequest = 0x09 (SET_REPORT)
    ///   - wValue = (report_type << 8) | report_id  (type 0x02 = output report)
    ///   - wIndex = interface number
    ///   - wLength = report length
    async fn set_report(&mut self, report: &[u8]) -> Result<(), ClientError> {
        let setup = build_set_report_setup(self.interface_number, report.len() as u16);
        self.control_transfer_out(&setup, report).await
    }

    // ── USB/IP transport helpers ────────────────────────────────────

    /// Allocate the next sequence number.
    fn next_seqnum(&mut self) -> u32 {
        let seq = self.next_seqnum;
        self.next_seqnum = self.next_seqnum.wrapping_add(1);
        if self.next_seqnum == 0 {
            self.next_seqnum = 1;
        }
        seq
    }

    /// Send a control IN transfer (EP0, direction=IN) and return the response data.
    async fn control_transfer_in(
        &mut self,
        setup: &[u8; 8],
        length: u32,
    ) -> Result<Vec<u8>, ClientError> {
        let seqnum = self.next_seqnum();

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 1, // IN
                ep: 0,        // control endpoint
            },
            transfer_flags: 0,
            transfer_buffer_length: length,
            start_frame: 0,
            number_of_packets: 0xFFFF_FFFF,
            interval: 0,
            setup: *setup,
            transfer_buffer: Bytes::new(),
            iso_packet_descriptors: vec![],
        };

        let mut buf = BytesMut::new();
        cmd.encode(&mut buf);
        self.writer.write_all(&buf).await.map_err(ClientError::Io)?;

        let ret = self.read_ret_submit().await?;
        Ok(ret)
    }

    /// Send a control OUT transfer (EP0, direction=OUT).
    async fn control_transfer_out(
        &mut self,
        setup: &[u8; 8],
        data: &[u8],
    ) -> Result<(), ClientError> {
        let seqnum = self.next_seqnum();

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 0, // OUT
                ep: 0,        // control endpoint
            },
            transfer_flags: 0,
            transfer_buffer_length: data.len() as u32,
            start_frame: 0,
            number_of_packets: 0xFFFF_FFFF,
            interval: 0,
            setup: *setup,
            transfer_buffer: Bytes::copy_from_slice(data),
            iso_packet_descriptors: vec![],
        };

        let mut buf = BytesMut::new();
        cmd.encode(&mut buf);
        self.writer.write_all(&buf).await.map_err(ClientError::Io)?;

        self.read_ret_submit().await?;
        Ok(())
    }

    /// Send an interrupt IN request and read the response data from RetSubmit.
    async fn receive_interrupt_in(&mut self, length: u32) -> Result<Vec<u8>, ClientError> {
        let seqnum = self.next_seqnum();
        let ep = self.ep_in & 0x0F; // strip direction bit

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 1, // IN
                ep: ep as u32,
            },
            transfer_flags: 0,
            transfer_buffer_length: length,
            start_frame: 0,
            number_of_packets: 0xFFFF_FFFF,
            interval: 0,
            setup: [0u8; 8],
            transfer_buffer: Bytes::new(),
            iso_packet_descriptors: vec![],
        };

        let mut buf = BytesMut::new();
        cmd.encode(&mut buf);
        self.writer.write_all(&buf).await.map_err(ClientError::Io)?;

        let ret = self.read_ret_submit().await?;
        Ok(ret)
    }

    /// Send an interrupt OUT transfer.
    async fn send_interrupt_out(&mut self, ep_out: u8, payload: &[u8]) -> Result<(), ClientError> {
        let seqnum = self.next_seqnum();
        let ep = ep_out & 0x0F; // strip direction bit

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 0, // OUT
                ep: ep as u32,
            },
            transfer_flags: 0,
            transfer_buffer_length: payload.len() as u32,
            start_frame: 0,
            number_of_packets: 0xFFFF_FFFF,
            interval: 0,
            setup: [0u8; 8],
            transfer_buffer: Bytes::copy_from_slice(payload),
            iso_packet_descriptors: vec![],
        };

        let mut buf = BytesMut::new();
        cmd.encode(&mut buf);
        self.writer.write_all(&buf).await.map_err(ClientError::Io)?;

        Ok(())
    }

    /// Read a RetSubmit message from the USB/IP stream, returning the transfer buffer.
    async fn read_ret_submit(&mut self) -> Result<Vec<u8>, ClientError> {
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::Hid(format!(
                        "URB transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(ret.transfer_buffer.to_vec())
            }
            other => Err(ClientError::Hid(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }
}

// ── Setup packet builders (public for testing) ──────────────────────

/// Build the setup packet for GET_DESCRIPTOR (HID report descriptor).
///
/// Returns an 8-byte USB setup packet.
pub(crate) fn build_get_report_descriptor_setup(interface: u8, length: u16) -> [u8; 8] {
    [
        USB_RT_HID_INTERFACE_IN, // bmRequestType: device-to-host, standard, interface
        USB_REQUEST_GET_DESCRIPTOR, // bRequest: GET_DESCRIPTOR
        0x00,                    // wValue low: descriptor index (0)
        HID_DESCRIPTOR_TYPE_REPORT, // wValue high: HID Report Descriptor (0x22)
        interface,               // wIndex low: interface number
        0x00,                    // wIndex high
        (length & 0xFF) as u8,   // wLength low
        ((length >> 8) & 0xFF) as u8, // wLength high
    ]
}

/// Build the setup packet for SET_IDLE.
///
/// Returns an 8-byte USB setup packet.
pub(crate) fn build_set_idle_setup(interface: u8, duration: u8, report_id: u8) -> [u8; 8] {
    [
        USB_RT_HID_INTERFACE_OUT, // bmRequestType: host-to-device, class, interface
        HID_REQUEST_SET_IDLE,     // bRequest: SET_IDLE
        report_id,                // wValue low: report ID
        duration,                 // wValue high: duration
        interface,                // wIndex low: interface number
        0x00,                     // wIndex high
        0x00,                     // wLength low
        0x00,                     // wLength high
    ]
}

/// Build the setup packet for SET_PROTOCOL.
///
/// Returns an 8-byte USB setup packet.
pub(crate) fn build_set_protocol_setup(interface: u8, protocol: u8) -> [u8; 8] {
    [
        USB_RT_HID_INTERFACE_OUT, // bmRequestType: host-to-device, class, interface
        HID_REQUEST_SET_PROTOCOL, // bRequest: SET_PROTOCOL
        protocol,                 // wValue low: protocol (0=boot, 1=report)
        0x00,                     // wValue high
        interface,                // wIndex low: interface number
        0x00,                     // wIndex high
        0x00,                     // wLength low
        0x00,                     // wLength high
    ]
}

/// Build the setup packet for SET_REPORT (output report, report_id=0).
///
/// Returns an 8-byte USB setup packet.
pub(crate) fn build_set_report_setup(interface: u8, length: u16) -> [u8; 8] {
    [
        USB_RT_HID_INTERFACE_OUT, // bmRequestType: host-to-device, class, interface
        HID_REQUEST_SET_REPORT,   // bRequest: SET_REPORT
        0x00,                     // wValue low: report ID (0)
        0x02,                     // wValue high: report type (0x02 = output)
        interface,                // wIndex low: interface number
        0x00,                     // wIndex high
        (length & 0xFF) as u8,    // wLength low
        ((length >> 8) & 0xFF) as u8, // wLength high
    ]
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Setup packet construction ───────────────────────────────────

    #[test]
    fn test_get_report_descriptor_setup() {
        let setup = build_get_report_descriptor_setup(0, 256);
        assert_eq!(setup[0], 0x81); // bmRequestType
        assert_eq!(setup[1], 0x06); // GET_DESCRIPTOR
        assert_eq!(setup[2], 0x00); // descriptor index
        assert_eq!(setup[3], 0x22); // HID Report Descriptor type
        assert_eq!(setup[4], 0x00); // interface 0
        assert_eq!(setup[5], 0x00);
        assert_eq!(setup[6], 0x00); // length low (256 = 0x0100)
        assert_eq!(setup[7], 0x01); // length high
    }

    #[test]
    fn test_get_report_descriptor_setup_interface_2() {
        let setup = build_get_report_descriptor_setup(2, 512);
        assert_eq!(setup[4], 2); // interface 2
                                 // 512 = 0x0200
        assert_eq!(setup[6], 0x00);
        assert_eq!(setup[7], 0x02);
    }

    #[test]
    fn test_get_report_descriptor_setup_small_length() {
        let setup = build_get_report_descriptor_setup(0, 64);
        assert_eq!(setup[6], 64); // length low
        assert_eq!(setup[7], 0); // length high
    }

    #[test]
    fn test_set_idle_setup() {
        let setup = build_set_idle_setup(0, 0, 0);
        assert_eq!(setup[0], 0x21); // bmRequestType
        assert_eq!(setup[1], 0x0A); // SET_IDLE
        assert_eq!(setup[2], 0x00); // report_id
        assert_eq!(setup[3], 0x00); // duration
        assert_eq!(setup[4], 0x00); // interface
        assert_eq!(setup[5], 0x00);
        assert_eq!(setup[6], 0x00); // wLength = 0
        assert_eq!(setup[7], 0x00);
    }

    #[test]
    fn test_set_idle_setup_with_params() {
        let setup = build_set_idle_setup(1, 100, 5);
        assert_eq!(setup[0], 0x21);
        assert_eq!(setup[1], 0x0A);
        assert_eq!(setup[2], 5); // report_id
        assert_eq!(setup[3], 100); // duration
        assert_eq!(setup[4], 1); // interface
    }

    #[test]
    fn test_set_protocol_setup_boot() {
        let setup = build_set_protocol_setup(0, 0);
        assert_eq!(setup[0], 0x21); // bmRequestType
        assert_eq!(setup[1], 0x0B); // SET_PROTOCOL
        assert_eq!(setup[2], 0x00); // boot protocol
        assert_eq!(setup[3], 0x00);
        assert_eq!(setup[4], 0x00); // interface
        assert_eq!(setup[6], 0x00); // wLength = 0
        assert_eq!(setup[7], 0x00);
    }

    #[test]
    fn test_set_protocol_setup_report() {
        let setup = build_set_protocol_setup(0, 1);
        assert_eq!(setup[2], 1); // report protocol
    }

    #[test]
    fn test_set_protocol_setup_interface_3() {
        let setup = build_set_protocol_setup(3, 1);
        assert_eq!(setup[4], 3); // interface 3
    }

    #[test]
    fn test_set_report_setup() {
        let setup = build_set_report_setup(0, 1);
        assert_eq!(setup[0], 0x21); // bmRequestType
        assert_eq!(setup[1], 0x09); // SET_REPORT
        assert_eq!(setup[2], 0x00); // report_id = 0
        assert_eq!(setup[3], 0x02); // report type = output
        assert_eq!(setup[4], 0x00); // interface
        assert_eq!(setup[5], 0x00);
        assert_eq!(setup[6], 0x01); // length low
        assert_eq!(setup[7], 0x00); // length high
    }

    #[test]
    fn test_set_report_setup_larger_payload() {
        let setup = build_set_report_setup(1, 300);
        assert_eq!(setup[4], 1); // interface 1
                                 // 300 = 0x012C
        assert_eq!(setup[6], 0x2C);
        assert_eq!(setup[7], 0x01);
    }

    // ── Integration test with mock HID server ───────────────────────

    #[tokio::test]
    async fn test_mock_hid_initialize() {
        use extender_protocol::codec::write_urb_message;
        use extender_protocol::device::UsbDevice;
        use extender_protocol::urb::RetSubmit;
        use extender_protocol::{OpMessage, OpRepImport};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // A minimal HID report descriptor for a keyboard (boot protocol).
        let fake_report_descriptor: Vec<u8> = vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x06, // Usage (Keyboard)
            0xA1, 0x01, // Collection (Application)
            0x05, 0x07, //   Usage Page (Key Codes)
            0x19, 0xE0, //   Usage Minimum (224)
            0x29, 0xE7, //   Usage Maximum (231)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x01, //   Logical Maximum (1)
            0x75, 0x01, //   Report Size (1)
            0x95, 0x08, //   Report Count (8)
            0x81, 0x02, //   Input (Data, Variable, Absolute) -- modifier byte
            0x95, 0x01, //   Report Count (1)
            0x75, 0x08, //   Report Size (8)
            0x81, 0x01, //   Input (Constant) -- reserved byte
            0x95, 0x06, //   Report Count (6)
            0x75, 0x08, //   Report Size (8)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x65, //   Logical Maximum (101)
            0x05, 0x07, //   Usage Page (Key Codes)
            0x19, 0x00, //   Usage Minimum (0)
            0x29, 0x65, //   Usage Maximum (101)
            0x81, 0x00, //   Input (Data, Array) -- keycodes
            0xC0, // End Collection
        ];

        let descriptor_clone = fake_report_descriptor.clone();

        // Mock server
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // 1) Read OP_REQ_IMPORT
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqImport(_)));

            // 2) Send OP_REP_IMPORT with a HID device descriptor
            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 2,
                speed: 2, // full speed
                id_vendor: 0x046D,
                id_product: 0xC534,
                bcd_device: 0x2901,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configuration_value: 1,
                num_configurations: 1,
                num_interfaces: 1,
                interfaces: vec![],
            };
            let rep = OpMessage::RepImport(Box::new(OpRepImport {
                status: 0,
                device: Some(device),
            }));
            write_op_message(&mut writer, &rep).await.unwrap();

            // 3) GET_DESCRIPTOR (HID report descriptor) - control IN on EP0
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 1); // IN
                assert_eq!(cmd.header.ep, 0); // EP0 (control)
                                              // Verify setup packet
                assert_eq!(cmd.setup[0], 0x81); // bmRequestType
                assert_eq!(cmd.setup[1], 0x06); // GET_DESCRIPTOR
                assert_eq!(cmd.setup[3], 0x22); // HID Report Descriptor

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: 0,
                    },
                    status: 0,
                    actual_length: descriptor_clone.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::from(descriptor_clone.clone()),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            } else {
                panic!("expected CmdSubmit for GET_DESCRIPTOR");
            }

            // 4) SET_IDLE - control OUT on EP0
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 0); // EP0
                assert_eq!(cmd.setup[0], 0x21); // bmRequestType
                assert_eq!(cmd.setup[1], 0x0A); // SET_IDLE

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: 0,
                    },
                    status: 0,
                    actual_length: 0,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            } else {
                panic!("expected CmdSubmit for SET_IDLE");
            }

            // 5) SET_PROTOCOL - control OUT on EP0
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 0); // EP0
                assert_eq!(cmd.setup[0], 0x21); // bmRequestType
                assert_eq!(cmd.setup[1], 0x0B); // SET_PROTOCOL
                assert_eq!(cmd.setup[2], 1); // report protocol

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: 0,
                    },
                    status: 0,
                    actual_length: 0,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            } else {
                panic!("expected CmdSubmit for SET_PROTOCOL");
            }
        });

        // Client side
        let mut dev = HidDevice::connect(addr, "1-1").await.unwrap();
        dev.initialize().await.unwrap();

        assert_eq!(dev.report_descriptor(), &fake_report_descriptor[..]);
        assert_eq!(dev.vendor_id(), 0x046D);
        assert_eq!(dev.product_id(), 0xC534);
        assert_eq!(dev.interface_number(), 0);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_hid_read_and_write_report() {
        use extender_protocol::codec::write_urb_message;
        use extender_protocol::device::UsbDevice;
        use extender_protocol::urb::RetSubmit;
        use extender_protocol::{OpMessage, OpRepImport};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Mock keyboard input report: modifier=0x02 (left shift), key=0x04 (A)
        let input_report: Vec<u8> = vec![0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00];
        let input_report_clone = input_report.clone();

        // Mock server
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // 1) OP_REQ_IMPORT / OP_REP_IMPORT
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqImport(_)));

            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 2,
                speed: 2,
                id_vendor: 0x046D,
                id_product: 0xC534,
                bcd_device: 0x2901,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configuration_value: 1,
                num_configurations: 1,
                num_interfaces: 1,
                interfaces: vec![],
            };
            let rep = OpMessage::RepImport(Box::new(OpRepImport {
                status: 0,
                device: Some(device),
            }));
            write_op_message(&mut writer, &rep).await.unwrap();

            // 2) read_report: interrupt IN on ep_in
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 1); // IN
                assert_eq!(cmd.header.ep, 1); // ep_in = 0x81, stripped = 1

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: input_report_clone.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::from(input_report_clone.clone()),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            } else {
                panic!("expected CmdSubmit for interrupt IN");
            }

            // 3) write_report: interrupt OUT on ep_out
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 2); // ep_out = 0x02, stripped = 2
                                              // Verify the LED output report was sent
                assert_eq!(cmd.transfer_buffer.as_ref(), &[0x02]); // caps lock LED

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: cmd.transfer_buffer_length,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            } else {
                panic!("expected CmdSubmit for interrupt OUT");
            }
        });

        // Client side
        let mut dev = HidDevice::connect(addr, "1-1").await.unwrap();

        // Read an input report
        let report = dev.read_report().await.unwrap();
        assert_eq!(report, input_report);

        // Write an output report (caps lock LED)
        dev.write_report(&[0x02]).await.unwrap();

        server.await.unwrap();
    }
}
