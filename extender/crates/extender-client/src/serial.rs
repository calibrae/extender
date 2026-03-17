//! USB CDC/ACM (Communications Device Class / Abstract Control Model) serial handler.
//!
//! This module implements the USB CDC/ACM protocol over USB/IP. CDC/ACM is how
//! USB serial adapters (FTDI, CP2102, Arduino, ESP32) communicate. The device
//! exposes two interfaces: a control interface (class 0x02, subclass 0x02) with
//! an optional interrupt IN endpoint for serial state notifications, and a data
//! interface (class 0x0A) with bulk IN + bulk OUT endpoints for serial data.

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

/// Default bulk OUT endpoint for data interface.
const DEFAULT_EP_DATA_OUT: u8 = 0x02;
/// Default bulk IN endpoint for data interface.
const DEFAULT_EP_DATA_IN: u8 = 0x82;

/// Connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// Size of LineCoding structure in bytes.
const LINE_CODING_SIZE: usize = 7;

// ── CDC/ACM class-specific request codes ─────────────────────────────

/// SET_LINE_CODING request code (0x20).
const SET_LINE_CODING: u8 = 0x20;
/// SET_CONTROL_LINE_STATE request code (0x22).
const SET_CONTROL_LINE_STATE: u8 = 0x22;

/// CDC/ACM class request type (host-to-device, class, interface).
const CDC_REQUEST_TYPE: u8 = 0x21;

// ── LineCoding ──────────────────────────────────────────────────────

/// USB CDC line coding structure (7 bytes, little-endian).
///
/// Describes the serial port configuration: baud rate, stop bits, parity,
/// and data bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCoding {
    /// Baud rate in bits per second (little-endian).
    pub baud_rate: u32,
    /// Stop bits: 0 = 1 stop bit, 1 = 1.5 stop bits, 2 = 2 stop bits.
    pub stop_bits: u8,
    /// Parity: 0 = none, 1 = odd, 2 = even, 3 = mark, 4 = space.
    pub parity: u8,
    /// Data bits: 5, 6, 7, or 8.
    pub data_bits: u8,
}

impl LineCoding {
    /// Serialize to a 7-byte little-endian buffer.
    pub fn to_bytes(&self) -> [u8; LINE_CODING_SIZE] {
        let mut buf = [0u8; LINE_CODING_SIZE];
        buf[0..4].copy_from_slice(&self.baud_rate.to_le_bytes());
        buf[4] = self.stop_bits;
        buf[5] = self.parity;
        buf[6] = self.data_bits;
        buf
    }

    /// Deserialize from a 7-byte little-endian buffer.
    pub fn from_bytes(buf: &[u8; LINE_CODING_SIZE]) -> Self {
        LineCoding {
            baud_rate: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            stop_bits: buf[4],
            parity: buf[5],
            data_bits: buf[6],
        }
    }
}

// ── SerialDevice ────────────────────────────────────────────────────

/// A USB CDC/ACM serial device accessed over USB/IP.
///
/// Wraps a TCP connection to a USB/IP server and provides serial read/write
/// operations using the CDC/ACM protocol.
pub struct SerialDevice {
    /// Read half of the TCP stream (after OP_REQ_IMPORT).
    reader: OwnedReadHalf,
    /// Write half of the TCP stream.
    writer: OwnedWriteHalf,
    /// Device ID from the IMPORT response.
    devid: u32,
    /// Bulk IN endpoint for serial data (e.g., 0x82).
    ep_data_in: u8,
    /// Bulk OUT endpoint for serial data (e.g., 0x02).
    ep_data_out: u8,
    /// Optional interrupt IN endpoint for serial state notifications.
    ep_notify: Option<u8>,
    /// Next sequence number for USB/IP URBs.
    next_seqnum: u32,
    /// Control interface number.
    control_interface: u8,
    /// Data interface number.
    data_interface: u8,
    /// Current baud rate.
    baud_rate: u32,
    /// Current data bits setting.
    data_bits: u8,
    /// Current stop bits setting.
    stop_bits: u8,
    /// Current parity setting.
    parity: u8,
}

impl SerialDevice {
    /// Connect to a USB/IP server and import a CDC/ACM serial device.
    ///
    /// Sends OP_REQ_IMPORT for the given `busid` and, on success,
    /// returns a `SerialDevice` ready for `initialize()`.
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
        let devid = match reply {
            OpMessage::RepImport(rep) => {
                if rep.status != 0 {
                    return Err(ClientError::ImportRejected {
                        busid: busid.to_string(),
                        status: rep.status,
                    });
                }
                let device = rep.device.ok_or(ClientError::ImportMissingDevice)?;
                (device.busnum << 16) | device.devnum
            }
            _ => {
                return Err(ClientError::Protocol(
                    extender_protocol::ProtocolError::InvalidOpCode(0),
                ));
            }
        };

        Ok(SerialDevice {
            reader,
            writer,
            devid,
            ep_data_in: DEFAULT_EP_DATA_IN,
            ep_data_out: DEFAULT_EP_DATA_OUT,
            ep_notify: None,
            next_seqnum: 1,
            control_interface: 0,
            data_interface: 1,
            baud_rate: 9600,
            data_bits: 8,
            stop_bits: 0,
            parity: 0,
        })
    }

    /// Initialize the serial device with the given baud rate.
    ///
    /// Sends SET_LINE_CODING and SET_CONTROL_LINE_STATE (DTR + RTS asserted)
    /// to configure the serial port.
    pub async fn initialize(&mut self, baud_rate: u32) -> Result<(), ClientError> {
        let coding = LineCoding {
            baud_rate,
            stop_bits: 0, // 1 stop bit
            parity: 0,    // no parity
            data_bits: 8, // 8 data bits
        };
        self.set_line_coding(&coding).await?;

        // Assert DTR and RTS
        self.set_control_line_state(true, true).await?;

        Ok(())
    }

    /// Read serial data from the device.
    ///
    /// Sends a bulk IN transfer on the data endpoint and returns the number
    /// of bytes read into `buf`.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ClientError> {
        let data = self
            .receive_bulk_in(self.ep_data_in, buf.len() as u32)
            .await?;
        let copy_len = data.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&data[..copy_len]);
        Ok(copy_len)
    }

    /// Write serial data to the device.
    ///
    /// Sends a bulk OUT transfer on the data endpoint and returns the number
    /// of bytes written.
    pub async fn write(&mut self, data: &[u8]) -> Result<usize, ClientError> {
        self.send_bulk_out(self.ep_data_out, data).await?;
        let ret = self.read_ret_submit().await?;
        let _ = ret;
        Ok(data.len())
    }

    /// Set the line coding (baud rate, stop bits, parity, data bits).
    ///
    /// Sends a SET_LINE_CODING class-specific control request on EP0.
    pub async fn set_line_coding(&mut self, coding: &LineCoding) -> Result<(), ClientError> {
        let iface = self.control_interface;
        let setup = [
            CDC_REQUEST_TYPE,       // bmRequestType: host-to-device, class, interface
            SET_LINE_CODING,        // bRequest
            0x00,                   // wValue low
            0x00,                   // wValue high
            iface,                  // wIndex low (interface)
            0x00,                   // wIndex high
            LINE_CODING_SIZE as u8, // wLength low
            0x00,                   // wLength high
        ];

        let payload = coding.to_bytes();
        self.send_control_out(&setup, &payload).await?;
        self.read_ret_submit().await?;

        self.baud_rate = coding.baud_rate;
        self.data_bits = coding.data_bits;
        self.stop_bits = coding.stop_bits;
        self.parity = coding.parity;

        Ok(())
    }

    /// Set the control line state (DTR, RTS).
    ///
    /// Sends a SET_CONTROL_LINE_STATE class-specific control request on EP0.
    pub async fn set_control_line_state(
        &mut self,
        dtr: bool,
        rts: bool,
    ) -> Result<(), ClientError> {
        let state: u16 = (dtr as u16) | ((rts as u16) << 1);
        let iface = self.control_interface;
        let setup = [
            CDC_REQUEST_TYPE,            // bmRequestType
            SET_CONTROL_LINE_STATE,      // bRequest
            (state & 0xFF) as u8,        // wValue low
            ((state >> 8) & 0xFF) as u8, // wValue high
            iface,                       // wIndex low (interface)
            0x00,                        // wIndex high
            0x00,                        // wLength low
            0x00,                        // wLength high
        ];

        self.send_control_out(&setup, &[]).await?;
        self.read_ret_submit().await?;

        Ok(())
    }

    /// Get the current baud rate.
    pub fn baud_rate(&self) -> u32 {
        self.baud_rate
    }

    /// Get the current data bits setting.
    pub fn data_bits(&self) -> u8 {
        self.data_bits
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Allocate the next sequence number.
    fn next_seqnum(&mut self) -> u32 {
        let seq = self.next_seqnum;
        self.next_seqnum = self.next_seqnum.wrapping_add(1);
        if self.next_seqnum == 0 {
            self.next_seqnum = 1;
        }
        seq
    }

    /// Send a control OUT transfer on EP0 with the given setup packet and optional data.
    async fn send_control_out(&mut self, setup: &[u8; 8], data: &[u8]) -> Result<(), ClientError> {
        let seqnum = self.next_seqnum();

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 0, // OUT
                ep: 0,        // EP0 for control transfers
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

        Ok(())
    }

    /// Send a bulk OUT transfer on the given endpoint.
    async fn send_bulk_out(&mut self, endpoint: u8, payload: &[u8]) -> Result<(), ClientError> {
        let seqnum = self.next_seqnum();
        let ep = endpoint & 0x0F;

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

    /// Send a bulk IN request and read the response data from RetSubmit.
    async fn receive_bulk_in(&mut self, endpoint: u8, length: u32) -> Result<Vec<u8>, ClientError> {
        let seqnum = self.next_seqnum();
        let ep = endpoint & 0x0F;

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

    /// Read a RetSubmit message from the USB/IP stream, returning the transfer buffer.
    async fn read_ret_submit(&mut self) -> Result<Vec<u8>, ClientError> {
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::Serial(format!(
                        "URB transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(ret.transfer_buffer.to_vec())
            }
            other => Err(ClientError::Serial(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────

/// Build the setup packet bytes for SET_LINE_CODING.
pub fn build_set_line_coding_setup(interface: u8) -> [u8; 8] {
    [
        CDC_REQUEST_TYPE,
        SET_LINE_CODING,
        0x00,
        0x00,
        interface,
        0x00,
        LINE_CODING_SIZE as u8,
        0x00,
    ]
}

/// Build the setup packet bytes for SET_CONTROL_LINE_STATE.
pub fn build_set_control_line_state_setup(interface: u8, dtr: bool, rts: bool) -> [u8; 8] {
    let state: u16 = (dtr as u16) | ((rts as u16) << 1);
    [
        CDC_REQUEST_TYPE,
        SET_CONTROL_LINE_STATE,
        (state & 0xFF) as u8,
        ((state >> 8) & 0xFF) as u8,
        interface,
        0x00,
        0x00,
        0x00,
    ]
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── LineCoding serialization ────────────────────────────────────

    #[test]
    fn test_line_coding_to_bytes() {
        let coding = LineCoding {
            baud_rate: 115200,
            stop_bits: 0,
            parity: 0,
            data_bits: 8,
        };
        let bytes = coding.to_bytes();
        assert_eq!(bytes.len(), 7);
        // 115200 = 0x0001_C200 in little-endian: [0x00, 0xC2, 0x01, 0x00]
        assert_eq!(&bytes[0..4], &115200u32.to_le_bytes());
        assert_eq!(bytes[4], 0); // stop bits
        assert_eq!(bytes[5], 0); // parity
        assert_eq!(bytes[6], 8); // data bits
    }

    #[test]
    fn test_line_coding_roundtrip() {
        let coding = LineCoding {
            baud_rate: 9600,
            stop_bits: 2,
            parity: 1,
            data_bits: 7,
        };
        let bytes = coding.to_bytes();
        let decoded = LineCoding::from_bytes(&bytes);
        assert_eq!(decoded, coding);
    }

    #[test]
    fn test_line_coding_various_baud_rates() {
        for &baud in &[
            300u32, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 921600,
        ] {
            let coding = LineCoding {
                baud_rate: baud,
                stop_bits: 0,
                parity: 0,
                data_bits: 8,
            };
            let bytes = coding.to_bytes();
            let decoded = LineCoding::from_bytes(&bytes);
            assert_eq!(decoded.baud_rate, baud);
        }
    }

    #[test]
    fn test_line_coding_stop_bits_values() {
        // 0=1 stop bit, 1=1.5 stop bits, 2=2 stop bits
        for stop in 0..=2u8 {
            let coding = LineCoding {
                baud_rate: 9600,
                stop_bits: stop,
                parity: 0,
                data_bits: 8,
            };
            let bytes = coding.to_bytes();
            assert_eq!(bytes[4], stop);
        }
    }

    #[test]
    fn test_line_coding_parity_values() {
        // 0=none, 1=odd, 2=even, 3=mark, 4=space
        for parity in 0..=4u8 {
            let coding = LineCoding {
                baud_rate: 9600,
                stop_bits: 0,
                parity,
                data_bits: 8,
            };
            let bytes = coding.to_bytes();
            assert_eq!(bytes[5], parity);
        }
    }

    #[test]
    fn test_line_coding_data_bits_values() {
        for &bits in &[5u8, 6, 7, 8] {
            let coding = LineCoding {
                baud_rate: 9600,
                stop_bits: 0,
                parity: 0,
                data_bits: bits,
            };
            let bytes = coding.to_bytes();
            assert_eq!(bytes[6], bits);
        }
    }

    // ── Setup packet construction ───────────────────────────────────

    #[test]
    fn test_set_line_coding_setup_packet() {
        let setup = build_set_line_coding_setup(0);
        assert_eq!(setup[0], 0x21); // bmRequestType: class, interface, host-to-device
        assert_eq!(setup[1], 0x20); // bRequest: SET_LINE_CODING
        assert_eq!(setup[2], 0x00); // wValue low
        assert_eq!(setup[3], 0x00); // wValue high
        assert_eq!(setup[4], 0x00); // wIndex low (interface 0)
        assert_eq!(setup[5], 0x00); // wIndex high
        assert_eq!(setup[6], 0x07); // wLength low (7 bytes)
        assert_eq!(setup[7], 0x00); // wLength high
    }

    #[test]
    fn test_set_line_coding_setup_interface_1() {
        let setup = build_set_line_coding_setup(1);
        assert_eq!(setup[4], 1); // wIndex should be interface 1
    }

    #[test]
    fn test_set_control_line_state_dtr_only() {
        let setup = build_set_control_line_state_setup(0, true, false);
        assert_eq!(setup[0], 0x21);
        assert_eq!(setup[1], 0x22); // SET_CONTROL_LINE_STATE
        assert_eq!(setup[2], 0x01); // wValue low: DTR=1, RTS=0
        assert_eq!(setup[3], 0x00); // wValue high
        assert_eq!(setup[6], 0x00); // wLength = 0 (no data stage)
        assert_eq!(setup[7], 0x00);
    }

    #[test]
    fn test_set_control_line_state_rts_only() {
        let setup = build_set_control_line_state_setup(0, false, true);
        assert_eq!(setup[2], 0x02); // wValue low: DTR=0, RTS=1
        assert_eq!(setup[3], 0x00);
    }

    #[test]
    fn test_set_control_line_state_both() {
        let setup = build_set_control_line_state_setup(0, true, true);
        assert_eq!(setup[2], 0x03); // wValue low: DTR=1, RTS=1
        assert_eq!(setup[3], 0x00);
    }

    #[test]
    fn test_set_control_line_state_neither() {
        let setup = build_set_control_line_state_setup(0, false, false);
        assert_eq!(setup[2], 0x00); // wValue low: DTR=0, RTS=0
        assert_eq!(setup[3], 0x00);
    }

    #[test]
    fn test_set_control_line_state_interface_2() {
        let setup = build_set_control_line_state_setup(2, true, true);
        assert_eq!(setup[4], 2); // wIndex should be interface 2
    }

    // ── Mock server integration test ────────────────────────────────

    #[tokio::test]
    async fn test_mock_serial_initialize() {
        use extender_protocol::codec::write_urb_message;
        use extender_protocol::device::UsbDevice;
        use extender_protocol::urb::RetSubmit;
        use extender_protocol::{OpMessage, OpRepImport};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Mock server
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // 1) Read OP_REQ_IMPORT
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqImport(_)));

            // 2) Send OP_REP_IMPORT
            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 3,
                speed: 2,
                id_vendor: 0x10C4, // Silicon Labs CP2102
                id_product: 0xEA60,
                bcd_device: 0x0100,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configuration_value: 1,
                num_configurations: 1,
                num_interfaces: 2,
                interfaces: vec![],
            };
            let rep = OpMessage::RepImport(Box::new(OpRepImport {
                status: 0,
                device: Some(device),
            }));
            write_op_message(&mut writer, &rep).await.unwrap();

            // 3) SET_LINE_CODING: control OUT on EP0
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.ep, 0); // EP0
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.setup[0], 0x21); // class request
                assert_eq!(cmd.setup[1], 0x20); // SET_LINE_CODING
                                                // Verify the LineCoding payload
                assert_eq!(cmd.transfer_buffer.len(), 7);
                let mut lc_buf = [0u8; 7];
                lc_buf.copy_from_slice(&cmd.transfer_buffer);
                let lc = LineCoding::from_bytes(&lc_buf);
                assert_eq!(lc.baud_rate, 115200);
                assert_eq!(lc.data_bits, 8);

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: 0,
                    },
                    status: 0,
                    actual_length: 7,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }

            // 4) SET_CONTROL_LINE_STATE: control OUT on EP0
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.ep, 0);
                assert_eq!(cmd.header.direction, 0);
                assert_eq!(cmd.setup[0], 0x21);
                assert_eq!(cmd.setup[1], 0x22); // SET_CONTROL_LINE_STATE
                                                // wValue = 0x0003 (DTR=1, RTS=1)
                assert_eq!(cmd.setup[2], 0x03);
                assert_eq!(cmd.setup[3], 0x00);

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
            }
        });

        // Client side
        let mut dev = SerialDevice::connect(addr, "1-1").await.unwrap();
        dev.initialize(115200).await.unwrap();

        assert_eq!(dev.baud_rate(), 115200);
        assert_eq!(dev.data_bits(), 8);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_serial_write_read() {
        use extender_protocol::codec::write_urb_message;
        use extender_protocol::device::UsbDevice;
        use extender_protocol::urb::RetSubmit;
        use extender_protocol::{OpMessage, OpRepImport};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // OP_REQ_IMPORT
            let _msg = read_op_message(&mut reader).await.unwrap();
            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 3,
                speed: 2,
                id_vendor: 0x10C4,
                id_product: 0xEA60,
                bcd_device: 0x0100,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                configuration_value: 1,
                num_configurations: 1,
                num_interfaces: 2,
                interfaces: vec![],
            };
            let rep = OpMessage::RepImport(Box::new(OpRepImport {
                status: 0,
                device: Some(device),
            }));
            write_op_message(&mut writer, &rep).await.unwrap();

            // Write: bulk OUT on EP2
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 2); // EP2
                assert_eq!(cmd.transfer_buffer.as_ref(), b"Hello, Serial!");

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: cmd.transfer_buffer.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }

            // Read: bulk IN on EP2 (direction bit stripped)
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 1); // IN
                assert_eq!(cmd.header.ep, 2); // EP2 (0x82 & 0x0F)

                let response_data = b"Response!";
                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: response_data.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::from_static(response_data),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        // Client side
        let mut dev = SerialDevice::connect(addr, "1-1").await.unwrap();

        // Write
        let written = dev.write(b"Hello, Serial!").await.unwrap();
        assert_eq!(written, 14);

        // Read
        let mut buf = [0u8; 64];
        let read_len = dev.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..read_len], b"Response!");

        server.await.unwrap();
    }
}
