//! URB (USB Request Block) message types.
//!
//! These messages are used after the discovery phase, once a device
//! has been imported and the connection transitions to URB traffic.

use bytes::{Buf, BufMut, Bytes};

use crate::codes::Command;
use crate::error::ProtocolError;
use crate::wire::WireFormat;

/// ECONNRESET value used when an unlink is successful.
pub const ECONNRESET: i32 = -104;

/// Size of the common URB header (usbip_header_basic) in bytes.
pub const HEADER_BASIC_SIZE: usize = 20;

/// Size of CMD_SUBMIT specific fields (after the basic header).
pub const CMD_SUBMIT_FIELDS_SIZE: usize = 28;

/// Size of RET_SUBMIT specific fields (after the basic header).
pub const RET_SUBMIT_FIELDS_SIZE: usize = 28;

/// Size of CMD_UNLINK specific fields (after the basic header).
pub const CMD_UNLINK_FIELDS_SIZE: usize = 28;

/// Size of RET_UNLINK specific fields (after the basic header).
pub const RET_UNLINK_FIELDS_SIZE: usize = 28;

/// Maximum allowed transfer buffer length (1 MB).
/// Matches Linux kernel behavior. Prevents memory exhaustion from malicious peers.
pub const MAX_TRANSFER_BUFFER_LENGTH: u32 = 1_048_576;

/// Maximum number of devices in a DEVLIST reply.
pub const MAX_DEVICES_IN_DEVLIST: u32 = 256;

/// Total header size for all URB messages (basic + specific = 48 bytes).
pub const URB_HEADER_TOTAL_SIZE: usize = 48;

/// Common header for all URB messages (20 bytes).
///
/// | Offset | Length | Field     |
/// |--------|--------|-----------|
/// | 0      | 4      | command   |
/// | 4      | 4      | seqnum   |
/// | 8      | 4      | devid    |
/// | 0xC    | 4      | direction |
/// | 0x10   | 4      | ep       |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbipHeaderBasic {
    /// Command type (see `Command` enum).
    pub command: u32,
    /// Sequential number that identifies requests and their replies.
    pub seqnum: u32,
    /// Device ID.
    pub devid: u32,
    /// Transfer direction: 0 = OUT (host to device), 1 = IN (device to host).
    pub direction: u32,
    /// Endpoint number.
    pub ep: u32,
}

impl WireFormat for UsbipHeaderBasic {
    fn encode(&self, buf: &mut impl BufMut) {
        buf.put_u32(self.command);
        buf.put_u32(self.seqnum);
        buf.put_u32(self.devid);
        buf.put_u32(self.direction);
        buf.put_u32(self.ep);
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        if buf.remaining() < HEADER_BASIC_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: HEADER_BASIC_SIZE,
                available: buf.remaining(),
            });
        }
        Ok(UsbipHeaderBasic {
            command: buf.get_u32(),
            seqnum: buf.get_u32(),
            devid: buf.get_u32(),
            direction: buf.get_u32(),
            ep: buf.get_u32(),
        })
    }

    fn wire_size(&self) -> usize {
        HEADER_BASIC_SIZE
    }
}

// ── CMD_SUBMIT ──────────────────────────────────────────────────────

/// Submit a USB request block.
///
/// Header (48 bytes) = basic(20) + submit-specific(28):
///   transfer_flags(4) + buffer_length(4) + start_frame(4) +
///   number_of_packets(4) + interval(4) + setup(8) = 28
///
/// Followed by:
/// - Transfer buffer (buffer_length bytes, for OUT direction)
/// - ISO packet descriptors (if isochronous)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdSubmit {
    pub header: UsbipHeaderBasic,
    /// URB transfer flags.
    pub transfer_flags: u32,
    /// Length of the transfer buffer.
    pub transfer_buffer_length: u32,
    /// Start frame for isochronous transfers.
    pub start_frame: u32,
    /// Number of ISO packets (-1 for non-ISO).
    pub number_of_packets: u32,
    /// Polling interval.
    pub interval: u32,
    /// USB setup packet (8 bytes, for control transfers).
    pub setup: [u8; 8],
    /// Transfer buffer payload.
    pub transfer_buffer: Bytes,
}

impl WireFormat for CmdSubmit {
    fn encode(&self, buf: &mut impl BufMut) {
        self.header.encode(buf);
        buf.put_u32(self.transfer_flags);
        buf.put_u32(self.transfer_buffer_length);
        buf.put_u32(self.start_frame);
        buf.put_u32(self.number_of_packets);
        buf.put_u32(self.interval);
        buf.put_slice(&self.setup);
        if !self.transfer_buffer.is_empty() {
            buf.put_slice(&self.transfer_buffer);
        }
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        let header = UsbipHeaderBasic::decode(buf)?;
        if header.command != Command::CmdSubmit as u32 {
            return Err(ProtocolError::InvalidCommand(header.command));
        }

        if buf.remaining() < CMD_SUBMIT_FIELDS_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: CMD_SUBMIT_FIELDS_SIZE,
                available: buf.remaining(),
            });
        }

        let transfer_flags = buf.get_u32();
        let transfer_buffer_length = buf.get_u32();
        if transfer_buffer_length > MAX_TRANSFER_BUFFER_LENGTH {
            return Err(ProtocolError::TransferTooLarge {
                length: transfer_buffer_length,
                max: MAX_TRANSFER_BUFFER_LENGTH,
            });
        }
        let start_frame = buf.get_u32();
        let number_of_packets = buf.get_u32();
        let interval = buf.get_u32();

        let mut setup = [0u8; 8];
        buf.copy_to_slice(&mut setup);

        // For OUT transfers (direction == 0), the buffer follows the header.
        // For IN transfers (direction == 1), the buffer is empty in the request.
        let buf_len = if header.direction == 0 {
            transfer_buffer_length as usize
        } else {
            0
        };

        if buf.remaining() < buf_len {
            return Err(ProtocolError::BufferTooShort {
                needed: buf_len,
                available: buf.remaining(),
            });
        }

        let transfer_buffer = if buf_len > 0 {
            buf.copy_to_bytes(buf_len)
        } else {
            Bytes::new()
        };

        Ok(CmdSubmit {
            header,
            transfer_flags,
            transfer_buffer_length,
            start_frame,
            number_of_packets,
            interval,
            setup,
            transfer_buffer,
        })
    }

    fn wire_size(&self) -> usize {
        URB_HEADER_TOTAL_SIZE + self.transfer_buffer.len()
    }
}

// ── RET_SUBMIT ──────────────────────────────────────────────────────

/// Return the result of a submitted URB.
///
/// Header (48 bytes) = basic(20) + return-specific(28):
///   status(4) + actual_length(4) + start_frame(4) +
///   number_of_packets(4) + error_count(4) + padding(8) = 28
///
/// Followed by:
/// - Transfer buffer (actual_length bytes, for IN direction)
/// - ISO packet descriptors (if isochronous)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetSubmit {
    pub header: UsbipHeaderBasic,
    /// URB status (0 = success, negative = error).
    pub status: i32,
    /// Actual number of bytes transferred.
    pub actual_length: u32,
    /// Start frame for isochronous transfers.
    pub start_frame: u32,
    /// Number of ISO packets.
    pub number_of_packets: u32,
    /// Number of ISO errors.
    pub error_count: u32,
    /// Transfer buffer payload (for IN transfers).
    pub transfer_buffer: Bytes,
}

impl WireFormat for RetSubmit {
    fn encode(&self, buf: &mut impl BufMut) {
        self.header.encode(buf);
        buf.put_i32(self.status);
        buf.put_u32(self.actual_length);
        buf.put_u32(self.start_frame);
        buf.put_u32(self.number_of_packets);
        buf.put_u32(self.error_count);
        buf.put_u64(0); // 8 bytes padding
        if !self.transfer_buffer.is_empty() {
            buf.put_slice(&self.transfer_buffer);
        }
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        let header = UsbipHeaderBasic::decode(buf)?;
        if header.command != Command::RetSubmit as u32 {
            return Err(ProtocolError::InvalidCommand(header.command));
        }

        if buf.remaining() < RET_SUBMIT_FIELDS_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: RET_SUBMIT_FIELDS_SIZE,
                available: buf.remaining(),
            });
        }

        let status = buf.get_i32();
        let actual_length = buf.get_u32();
        if actual_length > MAX_TRANSFER_BUFFER_LENGTH {
            return Err(ProtocolError::TransferTooLarge {
                length: actual_length,
                max: MAX_TRANSFER_BUFFER_LENGTH,
            });
        }
        let start_frame = buf.get_u32();
        let number_of_packets = buf.get_u32();
        let error_count = buf.get_u32();
        let _padding = buf.get_u64();

        // For IN transfers (direction == 1), the buffer follows the header.
        let buf_len = if header.direction == 1 {
            actual_length as usize
        } else {
            0
        };

        if buf.remaining() < buf_len {
            return Err(ProtocolError::BufferTooShort {
                needed: buf_len,
                available: buf.remaining(),
            });
        }

        let transfer_buffer = if buf_len > 0 {
            buf.copy_to_bytes(buf_len)
        } else {
            Bytes::new()
        };

        Ok(RetSubmit {
            header,
            status,
            actual_length,
            start_frame,
            number_of_packets,
            error_count,
            transfer_buffer,
        })
    }

    fn wire_size(&self) -> usize {
        URB_HEADER_TOTAL_SIZE + self.transfer_buffer.len()
    }
}

// ── CMD_UNLINK ──────────────────────────────────────────────────────

/// Cancel a previously submitted URB.
///
/// Header (48 bytes) = basic(20) + unlink-specific(28):
///   unlink_seqnum(4) + padding(24)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdUnlink {
    pub header: UsbipHeaderBasic,
    /// Sequence number of the URB to cancel.
    pub unlink_seqnum: u32,
}

impl WireFormat for CmdUnlink {
    fn encode(&self, buf: &mut impl BufMut) {
        self.header.encode(buf);
        buf.put_u32(self.unlink_seqnum);
        // 24 bytes of padding
        buf.put_u64(0);
        buf.put_u64(0);
        buf.put_u64(0);
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        let header = UsbipHeaderBasic::decode(buf)?;
        if header.command != Command::CmdUnlink as u32 {
            return Err(ProtocolError::InvalidCommand(header.command));
        }

        if buf.remaining() < CMD_UNLINK_FIELDS_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: CMD_UNLINK_FIELDS_SIZE,
                available: buf.remaining(),
            });
        }

        let unlink_seqnum = buf.get_u32();
        // Skip 24 bytes of padding
        buf.advance(24);

        Ok(CmdUnlink {
            header,
            unlink_seqnum,
        })
    }

    fn wire_size(&self) -> usize {
        URB_HEADER_TOTAL_SIZE
    }
}

// ── RET_UNLINK ──────────────────────────────────────────────────────

/// Confirm URB cancellation.
///
/// Header (48 bytes) = basic(20) + unlink-specific(28):
///   status(4) + padding(24)
///
/// Status is -ECONNRESET (-104) if the URB was successfully cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetUnlink {
    pub header: UsbipHeaderBasic,
    /// Status: -ECONNRESET (-104) if successful, 0 if URB already completed.
    pub status: i32,
}

impl WireFormat for RetUnlink {
    fn encode(&self, buf: &mut impl BufMut) {
        self.header.encode(buf);
        buf.put_i32(self.status);
        // 24 bytes of padding
        buf.put_u64(0);
        buf.put_u64(0);
        buf.put_u64(0);
    }

    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError> {
        let header = UsbipHeaderBasic::decode(buf)?;
        if header.command != Command::RetUnlink as u32 {
            return Err(ProtocolError::InvalidCommand(header.command));
        }

        if buf.remaining() < RET_UNLINK_FIELDS_SIZE {
            return Err(ProtocolError::BufferTooShort {
                needed: RET_UNLINK_FIELDS_SIZE,
                available: buf.remaining(),
            });
        }

        let status = buf.get_i32();
        // Skip 24 bytes of padding
        buf.advance(24);

        Ok(RetUnlink { header, status })
    }

    fn wire_size(&self) -> usize {
        URB_HEADER_TOTAL_SIZE
    }
}

/// Enum over all URB-phase messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrbMessage {
    CmdSubmit(CmdSubmit),
    RetSubmit(RetSubmit),
    CmdUnlink(CmdUnlink),
    RetUnlink(RetUnlink),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_basic_header(command: Command) -> UsbipHeaderBasic {
        UsbipHeaderBasic {
            command: command as u32,
            seqnum: 1,
            devid: 2,
            direction: 0,
            ep: 0,
        }
    }

    #[test]
    fn test_header_basic_size() {
        let header = make_basic_header(Command::CmdSubmit);
        let mut buf = Vec::new();
        header.encode(&mut buf);
        assert_eq!(buf.len(), 20);
    }

    #[test]
    fn test_header_basic_roundtrip() {
        let header = UsbipHeaderBasic {
            command: Command::CmdSubmit as u32,
            seqnum: 42,
            devid: 0x00010002,
            direction: 1,
            ep: 3,
        };
        let mut buf = Vec::new();
        header.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = UsbipHeaderBasic::decode(&mut cursor).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn test_header_basic_big_endian() {
        let header = UsbipHeaderBasic {
            command: 0x00000001,
            seqnum: 0x00000042,
            devid: 0x00010002,
            direction: 0x00000001,
            ep: 0x00000003,
        };
        let mut buf = Vec::new();
        header.encode(&mut buf);

        // command = 0x00000001
        assert_eq!(&buf[0..4], &[0x00, 0x00, 0x00, 0x01]);
        // seqnum = 0x00000042
        assert_eq!(&buf[4..8], &[0x00, 0x00, 0x00, 0x42]);
        // devid = 0x00010002
        assert_eq!(&buf[8..12], &[0x00, 0x01, 0x00, 0x02]);
        // direction = 1
        assert_eq!(&buf[12..16], &[0x00, 0x00, 0x00, 0x01]);
        // ep = 3
        assert_eq!(&buf[16..20], &[0x00, 0x00, 0x00, 0x03]);
    }

    #[test]
    fn test_cmd_submit_no_buffer() {
        let msg = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 1, // IN
                ep: 0,
            },
            transfer_flags: 0,
            transfer_buffer_length: 64,
            start_frame: 0,
            number_of_packets: 0xFFFFFFFF,
            interval: 0,
            setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00],
            transfer_buffer: Bytes::new(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 48); // header only, no payload for IN request

        let mut cursor = &buf[..];
        let decoded = CmdSubmit::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_cmd_submit_with_out_buffer() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let msg = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 0, // OUT
                ep: 1,
            },
            transfer_flags: 0,
            transfer_buffer_length: 4,
            start_frame: 0,
            number_of_packets: 0xFFFFFFFF,
            interval: 0,
            setup: [0; 8],
            transfer_buffer: Bytes::from(data.clone()),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 48 + 4);

        let mut cursor = &buf[..];
        let decoded = CmdSubmit::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.transfer_buffer.as_ref(), &data[..]);
    }

    #[test]
    fn test_ret_submit_no_buffer() {
        let msg = RetSubmit {
            header: UsbipHeaderBasic {
                command: Command::RetSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 0, // OUT
                ep: 1,
            },
            status: 0,
            actual_length: 0,
            start_frame: 0,
            number_of_packets: 0xFFFFFFFF,
            error_count: 0,
            transfer_buffer: Bytes::new(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 48);

        let mut cursor = &buf[..];
        let decoded = RetSubmit::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_ret_submit_with_in_buffer() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let msg = RetSubmit {
            header: UsbipHeaderBasic {
                command: Command::RetSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 1, // IN
                ep: 0,
            },
            status: 0,
            actual_length: 8,
            start_frame: 0,
            number_of_packets: 0xFFFFFFFF,
            error_count: 0,
            transfer_buffer: Bytes::from(data.clone()),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 48 + 8);

        let mut cursor = &buf[..];
        let decoded = RetSubmit::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.transfer_buffer.as_ref(), &data[..]);
    }

    #[test]
    fn test_cmd_unlink_size() {
        let msg = CmdUnlink {
            header: UsbipHeaderBasic {
                command: Command::CmdUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            unlink_seqnum: 3,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 48);
    }

    #[test]
    fn test_cmd_unlink_roundtrip() {
        let msg = CmdUnlink {
            header: UsbipHeaderBasic {
                command: Command::CmdUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            unlink_seqnum: 3,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = CmdUnlink::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_ret_unlink_econnreset() {
        let msg = RetUnlink {
            header: UsbipHeaderBasic {
                command: Command::RetUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            status: ECONNRESET,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        assert_eq!(buf.len(), 48);

        // Verify the status is correctly encoded as a signed big-endian i32.
        // -104 = 0xFFFFFF98
        assert_eq!(&buf[20..24], &[0xFF, 0xFF, 0xFF, 0x98]);

        let mut cursor = &buf[..];
        let decoded = RetUnlink::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.status, -104);
    }

    #[test]
    fn test_ret_unlink_zero_status() {
        let msg = RetUnlink {
            header: UsbipHeaderBasic {
                command: Command::RetUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            status: 0,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = RetUnlink::decode(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.status, 0);
    }

    #[test]
    fn test_cmd_unlink_padding_is_zeroed() {
        let msg = CmdUnlink {
            header: UsbipHeaderBasic {
                command: Command::CmdUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            unlink_seqnum: 3,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        // Bytes 24..48 should be zero (padding)
        assert!(buf[24..48].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_wrong_command_rejected() {
        // Encode a CmdSubmit but try to decode as CmdUnlink
        let msg = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 1,
                ep: 0,
            },
            transfer_flags: 0,
            transfer_buffer_length: 0,
            start_frame: 0,
            number_of_packets: 0,
            interval: 0,
            setup: [0; 8],
            transfer_buffer: Bytes::new(),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let result = CmdUnlink::decode(&mut cursor);
        assert!(matches!(result, Err(ProtocolError::InvalidCommand(1))));
    }
}
