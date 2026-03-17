//! USB CDC-ECM/NCM (Ethernet/Network Control Model) protocol handler.
//!
//! This module implements USB CDC-ECM and CDC-NCM networking over USB/IP.
//! CDC-ECM sends raw Ethernet frames on bulk endpoints, while CDC-NCM wraps
//! frames in NTB (NDP Transfer Block) headers. Both protocols use a control
//! interface (class 0x02, subclass 0x06 for ECM or 0x0D for NCM) with an
//! interrupt IN endpoint for link notifications, and a data interface
//! (class 0x0A) with bulk IN + bulk OUT endpoints.

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
const DEFAULT_EP_DATA_IN: u8 = 0x81;

/// Connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// CDC class request type (host-to-device, class, interface).
const CDC_REQUEST_TYPE: u8 = 0x21;

/// SET_ETHERNET_PACKET_FILTER request code.
const SET_ETHERNET_PACKET_FILTER: u8 = 0x43;

/// Default max segment size for Ethernet frames.
const DEFAULT_MAX_SEGMENT_SIZE: u16 = 1514;

/// NTH16 signature "NCMH".
const NTH16_SIGNATURE: u32 = 0x484D434E;
/// NDP16 signature "NCM0".
const NDP16_SIGNATURE: u32 = 0x304D434E;
/// NTH16 header size.
const NTH16_HEADER_SIZE: usize = 12;
/// NDP16 minimum header size (signature + length + next_ndp_index + 2 datagram entries).
const NDP16_MIN_SIZE: usize = 16;

// ── Packet filter flags ─────────────────────────────────────────────

/// Accept all multicast packets.
pub const PACKET_TYPE_ALL_MULTICAST: u16 = 0x0004;
/// Accept directed (unicast) packets.
pub const PACKET_TYPE_DIRECTED: u16 = 0x0001;
/// Accept broadcast packets.
pub const PACKET_TYPE_BROADCAST: u16 = 0x0002;
/// Accept multicast packets matching the multicast address list.
pub const PACKET_TYPE_MULTICAST: u16 = 0x0010;
/// Promiscuous mode: accept all packets.
pub const PACKET_TYPE_PROMISCUOUS: u16 = 0x0008;

/// Default packet filter: directed + broadcast + multicast.
pub const DEFAULT_PACKET_FILTER: u16 =
    PACKET_TYPE_DIRECTED | PACKET_TYPE_BROADCAST | PACKET_TYPE_MULTICAST;

// ── NetworkProtocol ─────────────────────────────────────────────────

/// The CDC networking sub-protocol in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProtocol {
    /// CDC-ECM: raw Ethernet frames on bulk endpoints.
    Ecm,
    /// CDC-NCM: frames wrapped in NTB (NDP Transfer Block) headers.
    Ncm,
}

// ── NetworkDevice ───────────────────────────────────────────────────

/// A USB CDC-ECM/NCM network device accessed over USB/IP.
///
/// Wraps a TCP connection to a USB/IP server and provides Ethernet frame
/// read/write operations.
pub struct NetworkDevice {
    /// Read half of the TCP stream (after OP_REQ_IMPORT).
    reader: OwnedReadHalf,
    /// Write half of the TCP stream.
    writer: OwnedWriteHalf,
    /// Device ID from the IMPORT response.
    devid: u32,
    /// Bulk IN endpoint for Ethernet data.
    ep_data_in: u8,
    /// Bulk OUT endpoint for Ethernet data.
    ep_data_out: u8,
    /// Optional interrupt IN endpoint for link notifications.
    ep_notify: Option<u8>,
    /// Next sequence number for USB/IP URBs.
    next_seqnum: u32,
    /// The networking sub-protocol (ECM or NCM).
    protocol: NetworkProtocol,
    /// MAC address of the device.
    mac_address: [u8; 6],
    /// Maximum Ethernet segment size.
    max_segment_size: u16,
    /// Control interface number.
    control_interface: u8,
}

impl NetworkDevice {
    /// Connect to a USB/IP server and import a CDC-ECM/NCM network device.
    ///
    /// Sends OP_REQ_IMPORT for the given `busid` and, on success,
    /// returns a `NetworkDevice` ready for `initialize()`.
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

        Ok(NetworkDevice {
            reader,
            writer,
            devid,
            ep_data_in: DEFAULT_EP_DATA_IN,
            ep_data_out: DEFAULT_EP_DATA_OUT,
            ep_notify: None,
            next_seqnum: 1,
            protocol: NetworkProtocol::Ecm,
            mac_address: [0u8; 6],
            max_segment_size: DEFAULT_MAX_SEGMENT_SIZE,
            control_interface: 0,
        })
    }

    /// Initialize the network device.
    ///
    /// Sets the default packet filter to accept directed, broadcast, and
    /// multicast Ethernet frames.
    pub async fn initialize(&mut self) -> Result<(), ClientError> {
        self.set_packet_filter(DEFAULT_PACKET_FILTER).await?;
        Ok(())
    }

    /// Read an Ethernet frame from the device.
    ///
    /// For ECM, returns the raw Ethernet frame. For NCM, unwraps the NTB
    /// headers and returns the first datagram.
    pub async fn read_frame(&mut self) -> Result<Vec<u8>, ClientError> {
        let max_len = self.max_segment_size as u32 + 512; // extra room for NTB headers
        let data = self.receive_bulk_in(self.ep_data_in, max_len).await?;

        match self.protocol {
            NetworkProtocol::Ecm => Ok(data),
            NetworkProtocol::Ncm => Self::unwrap_ncm_ntb(&data),
        }
    }

    /// Write an Ethernet frame to the device.
    ///
    /// For ECM, sends the raw frame. For NCM, wraps it in NTB headers first.
    pub async fn write_frame(&mut self, frame: &[u8]) -> Result<(), ClientError> {
        let payload = match self.protocol {
            NetworkProtocol::Ecm => frame.to_vec(),
            NetworkProtocol::Ncm => Self::wrap_ncm_ntb(frame),
        };

        self.send_bulk_out(self.ep_data_out, &payload).await?;
        self.read_ret_submit().await?;
        Ok(())
    }

    /// Get the MAC address of the device.
    pub fn mac_address(&self) -> &[u8; 6] {
        &self.mac_address
    }

    /// Set the MAC address (used during configuration/discovery).
    pub fn set_mac_address(&mut self, mac: [u8; 6]) {
        self.mac_address = mac;
    }

    /// Set the networking protocol (ECM or NCM).
    pub fn set_protocol(&mut self, protocol: NetworkProtocol) {
        self.protocol = protocol;
    }

    /// Get the current protocol.
    pub fn protocol(&self) -> NetworkProtocol {
        self.protocol
    }

    /// Get the maximum segment size.
    pub fn max_segment_size(&self) -> u16 {
        self.max_segment_size
    }

    /// Set the Ethernet packet filter.
    ///
    /// Sends a SET_ETHERNET_PACKET_FILTER class-specific control request on EP0.
    pub async fn set_packet_filter(&mut self, filter: u16) -> Result<(), ClientError> {
        let iface = self.control_interface;
        let setup = build_set_packet_filter_setup(iface, filter);

        self.send_control_out(&setup, &[]).await?;
        self.read_ret_submit().await?;

        Ok(())
    }

    // ── NCM NTB helpers ─────────────────────────────────────────────

    /// Wrap an Ethernet frame in an NCM NTB16 structure.
    ///
    /// Layout: NTH16 (12 bytes) + NDP16 (16 bytes) + datagram
    fn wrap_ncm_ntb(frame: &[u8]) -> Vec<u8> {
        let ndp_offset: u16 = NTH16_HEADER_SIZE as u16;
        let datagram_offset: u16 = (NTH16_HEADER_SIZE + NDP16_MIN_SIZE) as u16;
        let datagram_len: u16 = frame.len() as u16;
        let block_len: u16 = datagram_offset + datagram_len;

        let mut buf = Vec::with_capacity(block_len as usize);

        // NTH16 header (12 bytes)
        buf.extend_from_slice(&NTH16_SIGNATURE.to_le_bytes()); // dwSignature
        buf.extend_from_slice(&12u16.to_le_bytes()); // wHeaderLength
        buf.extend_from_slice(&0u16.to_le_bytes()); // wSequence
        buf.extend_from_slice(&block_len.to_le_bytes()); // wBlockLength
        buf.extend_from_slice(&ndp_offset.to_le_bytes()); // wNdpIndex

        // NDP16 header (16 bytes)
        buf.extend_from_slice(&NDP16_SIGNATURE.to_le_bytes()); // dwSignature
        buf.extend_from_slice(&16u16.to_le_bytes()); // wLength
        buf.extend_from_slice(&0u16.to_le_bytes()); // wNextNdpIndex
                                                    // Datagram entry 0
        buf.extend_from_slice(&datagram_offset.to_le_bytes()); // wDatagramIndex
        buf.extend_from_slice(&datagram_len.to_le_bytes()); // wDatagramLength
                                                            // Terminator entry
        buf.extend_from_slice(&0u16.to_le_bytes()); // wDatagramIndex = 0
        buf.extend_from_slice(&0u16.to_le_bytes()); // wDatagramLength = 0

        // Datagram (the Ethernet frame)
        buf.extend_from_slice(frame);

        buf
    }

    /// Unwrap an NCM NTB16 structure, returning the first Ethernet frame.
    fn unwrap_ncm_ntb(data: &[u8]) -> Result<Vec<u8>, ClientError> {
        if data.len() < NTH16_HEADER_SIZE {
            return Err(ClientError::Network(
                "NTB too short for NTH16 header".to_string(),
            ));
        }

        // Verify NTH16 signature
        let sig = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if sig != NTH16_SIGNATURE {
            return Err(ClientError::Network(format!(
                "invalid NTH16 signature: 0x{sig:08X}"
            )));
        }

        // Get NDP offset
        let ndp_offset = u16::from_le_bytes([data[10], data[11]]) as usize;
        if ndp_offset + NDP16_MIN_SIZE > data.len() {
            return Err(ClientError::Network(
                "NTB too short for NDP16 header".to_string(),
            ));
        }

        // Verify NDP16 signature
        let ndp_sig = u32::from_le_bytes([
            data[ndp_offset],
            data[ndp_offset + 1],
            data[ndp_offset + 2],
            data[ndp_offset + 3],
        ]);
        if ndp_sig != NDP16_SIGNATURE {
            return Err(ClientError::Network(format!(
                "invalid NDP16 signature: 0x{ndp_sig:08X}"
            )));
        }

        // Read first datagram entry (offset 8 from NDP start)
        let entry_offset = ndp_offset + 8;
        if entry_offset + 4 > data.len() {
            return Err(ClientError::Network(
                "NDP16 too short for datagram entry".to_string(),
            ));
        }

        let datagram_index =
            u16::from_le_bytes([data[entry_offset], data[entry_offset + 1]]) as usize;
        let datagram_length =
            u16::from_le_bytes([data[entry_offset + 2], data[entry_offset + 3]]) as usize;

        if datagram_index == 0 || datagram_length == 0 {
            return Err(ClientError::Network(
                "NDP16 contains no datagrams".to_string(),
            ));
        }

        if datagram_index + datagram_length > data.len() {
            return Err(ClientError::Network(format!(
                "datagram extends past NTB: offset={datagram_index}, len={datagram_length}, total={}",
                data.len()
            )));
        }

        Ok(data[datagram_index..datagram_index + datagram_length].to_vec())
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

    /// Send a control OUT transfer on EP0.
    async fn send_control_out(&mut self, setup: &[u8; 8], data: &[u8]) -> Result<(), ClientError> {
        let seqnum = self.next_seqnum();

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 0, // OUT
                ep: 0,        // EP0
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
                    return Err(ClientError::Network(format!(
                        "URB transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(ret.transfer_buffer.to_vec())
            }
            other => Err(ClientError::Network(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────

/// Build the setup packet bytes for SET_ETHERNET_PACKET_FILTER.
pub fn build_set_packet_filter_setup(interface: u8, filter: u16) -> [u8; 8] {
    [
        CDC_REQUEST_TYPE,             // bmRequestType
        SET_ETHERNET_PACKET_FILTER,   // bRequest
        (filter & 0xFF) as u8,        // wValue low
        ((filter >> 8) & 0xFF) as u8, // wValue high
        interface,                    // wIndex low (interface)
        0x00,                         // wIndex high
        0x00,                         // wLength low
        0x00,                         // wLength high
    ]
}

/// Parse a MAC address from a colon-separated hex string (e.g., "AA:BB:CC:DD:EE:FF").
pub fn parse_mac_address(s: &str) -> Result<[u8; 6], ClientError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return Err(ClientError::Network(format!(
            "invalid MAC address format: {s}"
        )));
    }

    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(part, 16).map_err(|_| {
            ClientError::Network(format!("invalid hex byte in MAC address: {part}"))
        })?;
    }
    Ok(mac)
}

/// Format a MAC address as a colon-separated hex string.
pub fn format_mac_address(mac: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MAC address parsing ─────────────────────────────────────────

    #[test]
    fn test_parse_mac_address() {
        let mac = parse_mac_address("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(mac, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_mac_address_lowercase() {
        let mac = parse_mac_address("aa:bb:cc:dd:ee:ff").unwrap();
        assert_eq!(mac, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_parse_mac_address_zeros() {
        let mac = parse_mac_address("00:00:00:00:00:00").unwrap();
        assert_eq!(mac, [0x00; 6]);
    }

    #[test]
    fn test_parse_mac_address_broadcast() {
        let mac = parse_mac_address("FF:FF:FF:FF:FF:FF").unwrap();
        assert_eq!(mac, [0xFF; 6]);
    }

    #[test]
    fn test_parse_mac_address_invalid_format() {
        assert!(parse_mac_address("AA:BB:CC:DD:EE").is_err());
        assert!(parse_mac_address("AA:BB:CC:DD:EE:FF:00").is_err());
        assert!(parse_mac_address("AABBCCDDEEFF").is_err());
        assert!(parse_mac_address("").is_err());
    }

    #[test]
    fn test_parse_mac_address_invalid_hex() {
        assert!(parse_mac_address("GG:BB:CC:DD:EE:FF").is_err());
        assert!(parse_mac_address("AA:ZZ:CC:DD:EE:FF").is_err());
    }

    #[test]
    fn test_format_mac_address() {
        let mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        assert_eq!(format_mac_address(&mac), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn test_format_mac_address_zeros() {
        let mac = [0x00; 6];
        assert_eq!(format_mac_address(&mac), "00:00:00:00:00:00");
    }

    #[test]
    fn test_mac_address_roundtrip() {
        let original = "12:34:56:78:9A:BC";
        let mac = parse_mac_address(original).unwrap();
        assert_eq!(format_mac_address(&mac), original);
    }

    // ── Packet filter setup packet ──────────────────────────────────

    #[test]
    fn test_set_packet_filter_setup() {
        let setup = build_set_packet_filter_setup(0, DEFAULT_PACKET_FILTER);
        assert_eq!(setup[0], 0x21); // bmRequestType
        assert_eq!(setup[1], 0x43); // bRequest: SET_ETHERNET_PACKET_FILTER
                                    // wValue = DEFAULT_PACKET_FILTER = 0x0013
        let filter_val = DEFAULT_PACKET_FILTER;
        assert_eq!(setup[2], (filter_val & 0xFF) as u8);
        assert_eq!(setup[3], ((filter_val >> 8) & 0xFF) as u8);
        assert_eq!(setup[4], 0x00); // wIndex (interface 0)
        assert_eq!(setup[5], 0x00);
        assert_eq!(setup[6], 0x00); // wLength = 0
        assert_eq!(setup[7], 0x00);
    }

    #[test]
    fn test_set_packet_filter_promiscuous() {
        let setup = build_set_packet_filter_setup(1, PACKET_TYPE_PROMISCUOUS);
        assert_eq!(setup[2], 0x08); // wValue low
        assert_eq!(setup[3], 0x00); // wValue high
        assert_eq!(setup[4], 1); // interface 1
    }

    #[test]
    fn test_set_packet_filter_interface_2() {
        let setup = build_set_packet_filter_setup(2, PACKET_TYPE_DIRECTED);
        assert_eq!(setup[4], 2);
    }

    // ── NCM NTB wrap/unwrap ─────────────────────────────────────────

    #[test]
    fn test_ncm_ntb_wrap_unwrap_roundtrip() {
        let frame = vec![
            // Destination MAC
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // Source MAC
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, // EtherType (IPv4)
            0x08, 0x00, // Payload
            0x45, 0x00, 0x00, 0x1C, 0x00, 0x01,
        ];

        let ntb = NetworkDevice::wrap_ncm_ntb(&frame);
        let recovered = NetworkDevice::unwrap_ncm_ntb(&ntb).unwrap();
        assert_eq!(recovered, frame);
    }

    #[test]
    fn test_ncm_ntb_wrap_structure() {
        let frame = vec![0xAA; 20];
        let ntb = NetworkDevice::wrap_ncm_ntb(&frame);

        // NTH16: 12 bytes, NDP16: 16 bytes, datagram: 20 bytes
        assert_eq!(ntb.len(), 12 + 16 + 20);

        // Check NTH16 signature "NCMH"
        let sig = u32::from_le_bytes([ntb[0], ntb[1], ntb[2], ntb[3]]);
        assert_eq!(sig, NTH16_SIGNATURE);

        // Check header length
        let hdr_len = u16::from_le_bytes([ntb[4], ntb[5]]);
        assert_eq!(hdr_len, 12);

        // Check block length
        let block_len = u16::from_le_bytes([ntb[8], ntb[9]]);
        assert_eq!(block_len, 48);

        // Check NDP offset
        let ndp_offset = u16::from_le_bytes([ntb[10], ntb[11]]);
        assert_eq!(ndp_offset, 12);

        // Check NDP16 signature "NCM0"
        let ndp_sig = u32::from_le_bytes([ntb[12], ntb[13], ntb[14], ntb[15]]);
        assert_eq!(ndp_sig, NDP16_SIGNATURE);
    }

    #[test]
    fn test_ncm_ntb_unwrap_invalid_signature() {
        let mut data = vec![0u8; 48];
        // Invalid NTH16 signature
        data[0..4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(NetworkDevice::unwrap_ncm_ntb(&data).is_err());
    }

    #[test]
    fn test_ncm_ntb_unwrap_too_short() {
        let data = vec![0u8; 4]; // way too short
        assert!(NetworkDevice::unwrap_ncm_ntb(&data).is_err());
    }

    #[test]
    fn test_ncm_ntb_unwrap_empty_ntb() {
        // Valid headers but terminator entry (no datagrams)
        let mut data = vec![0u8; 28];
        // NTH16
        data[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
        data[4..6].copy_from_slice(&12u16.to_le_bytes());
        data[8..10].copy_from_slice(&28u16.to_le_bytes());
        data[10..12].copy_from_slice(&12u16.to_le_bytes()); // NDP at offset 12
                                                            // NDP16
        data[12..16].copy_from_slice(&NDP16_SIGNATURE.to_le_bytes());
        data[16..18].copy_from_slice(&16u16.to_le_bytes());
        // Terminator: datagram index=0, length=0
        data[20..22].copy_from_slice(&0u16.to_le_bytes());
        data[22..24].copy_from_slice(&0u16.to_le_bytes());

        assert!(NetworkDevice::unwrap_ncm_ntb(&data).is_err());
    }

    // ── ECM frame read/write mock test ──────────────────────────────

    #[tokio::test]
    async fn test_mock_ecm_frame_read_write() {
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
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-2"),
                busid: UsbDevice::busid_from_str("1-2").unwrap(),
                busnum: 1,
                devnum: 4,
                speed: 3,
                id_vendor: 0x0BDA, // Realtek
                id_product: 0x8153,
                bcd_device: 0x3000,
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

            // SET_ETHERNET_PACKET_FILTER (initialize)
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.ep, 0);
                assert_eq!(cmd.setup[0], 0x21);
                assert_eq!(cmd.setup[1], 0x43); // SET_ETHERNET_PACKET_FILTER

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

            // Write frame: bulk OUT
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 2);
                // Verify it's a raw Ethernet frame
                assert!(cmd.transfer_buffer.len() >= 14); // at least header

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

            // Read frame: bulk IN
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 1); // IN
                assert_eq!(cmd.header.ep, 1); // 0x81 & 0x0F

                // Return a fake Ethernet frame
                let mut eth_frame = vec![0u8; 60]; // minimum Ethernet frame
                                                   // Destination: broadcast
                eth_frame[0..6].copy_from_slice(&[0xFF; 6]);
                // Source: some MAC
                eth_frame[6..12].copy_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
                // EtherType: ARP
                eth_frame[12] = 0x08;
                eth_frame[13] = 0x06;

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: eth_frame.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::from(eth_frame),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        // Client side
        let mut dev = NetworkDevice::connect(addr, "1-2").await.unwrap();
        dev.initialize().await.unwrap();

        // Write an Ethernet frame
        let mut frame = vec![0u8; 60];
        frame[0..6].copy_from_slice(&[0xFF; 6]); // broadcast
        frame[6..12].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]);
        frame[12] = 0x08;
        frame[13] = 0x00; // IPv4
        dev.write_frame(&frame).await.unwrap();

        // Read an Ethernet frame
        let received = dev.read_frame().await.unwrap();
        assert_eq!(received.len(), 60);
        assert_eq!(&received[0..6], &[0xFF; 6]); // broadcast destination
        assert_eq!(&received[12..14], &[0x08, 0x06]); // ARP

        server.await.unwrap();
    }

    // ── Mock server integration: NCM protocol ───────────────────────

    #[tokio::test]
    async fn test_mock_ncm_frame_roundtrip() {
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
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-3"),
                busid: UsbDevice::busid_from_str("1-3").unwrap(),
                busnum: 1,
                devnum: 5,
                speed: 3,
                id_vendor: 0x0BDA,
                id_product: 0x8153,
                bcd_device: 0x3000,
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

            // SET_ETHERNET_PACKET_FILTER
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
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

            // Write frame (NCM-wrapped): bulk OUT
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                // Verify this is an NTB by checking the signature
                assert!(cmd.transfer_buffer.len() >= 12);
                let sig = u32::from_le_bytes([
                    cmd.transfer_buffer[0],
                    cmd.transfer_buffer[1],
                    cmd.transfer_buffer[2],
                    cmd.transfer_buffer[3],
                ]);
                assert_eq!(sig, NTH16_SIGNATURE);

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

            // Read frame: bulk IN -- return an NCM-wrapped frame
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                let mut eth_frame = vec![0u8; 64];
                eth_frame[0..6].copy_from_slice(&[0xFF; 6]);
                eth_frame[6..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
                eth_frame[12] = 0x08;
                eth_frame[13] = 0x00;

                let ntb = NetworkDevice::wrap_ncm_ntb(&eth_frame);

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: ntb.len() as u32,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::from(ntb),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        // Client side
        let mut dev = NetworkDevice::connect(addr, "1-3").await.unwrap();
        dev.set_protocol(NetworkProtocol::Ncm);
        dev.initialize().await.unwrap();

        // Write an NCM frame
        let mut frame = vec![0u8; 64];
        frame[0..6].copy_from_slice(&[0xFF; 6]);
        frame[6..12].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]);
        frame[12] = 0x08;
        frame[13] = 0x00;
        dev.write_frame(&frame).await.unwrap();

        // Read an NCM frame (should be unwrapped)
        let received = dev.read_frame().await.unwrap();
        assert_eq!(received.len(), 64);
        assert_eq!(&received[0..6], &[0xFF; 6]);
        assert_eq!(&received[6..12], &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);

        server.await.unwrap();
    }
}
