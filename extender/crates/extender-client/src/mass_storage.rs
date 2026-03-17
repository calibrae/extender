//! USB Mass Storage Bulk-Only Transport (BBB) protocol handler.
//!
//! This module implements the USB Mass Storage class Bulk-Only Transport
//! protocol over USB/IP. It wraps SCSI commands in CBW/CSW wrappers and
//! sends them as USB/IP URBs through a TCP connection to a remote server.

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

/// CBW signature: "USBC" in little-endian.
const CBW_SIGNATURE: u32 = 0x4342_5355;
/// CSW signature: "USBS" in little-endian.
const CSW_SIGNATURE: u32 = 0x5342_5355;
/// CBW size in bytes.
const CBW_SIZE: usize = 31;
/// CSW size in bytes.
const CSW_SIZE: usize = 13;

/// Default bulk OUT endpoint number (without direction bit).
const DEFAULT_EP_OUT: u8 = 0x02;
/// Default bulk IN endpoint number (with direction bit 0x80).
const DEFAULT_EP_IN: u8 = 0x81;

/// Connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 5;

// ── Direction ────────────────────────────────────────────────────────

/// Data transfer direction for a SCSI command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host to device (bulk OUT).
    Out = 0,
    /// Device to host (bulk IN).
    In = 1,
}

// ── CSW Status ───────────────────────────────────────────────────────

/// Status codes from a Command Status Wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CswStatus {
    /// Command completed successfully.
    Passed,
    /// Command failed; issue REQUEST SENSE for details.
    Failed,
    /// Phase error; device needs a reset.
    PhaseError,
    /// Unknown status code.
    Unknown(u8),
}

impl CswStatus {
    fn from_byte(b: u8) -> Self {
        match b {
            0 => CswStatus::Passed,
            1 => CswStatus::Failed,
            2 => CswStatus::PhaseError,
            other => CswStatus::Unknown(other),
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            CswStatus::Passed => 0,
            CswStatus::Failed => 1,
            CswStatus::PhaseError => 2,
            CswStatus::Unknown(b) => b,
        }
    }
}

// ── SCSI Commands ────────────────────────────────────────────────────

/// SCSI commands supported by this mass storage driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScsiCommand {
    /// TEST UNIT READY (0x00) -- check if device is ready.
    TestUnitReady,
    /// INQUIRY (0x12) -- get device info (vendor, product, etc.).
    Inquiry,
    /// READ CAPACITY (10) (0x25) -- get disk size and block size.
    ReadCapacity10,
    /// READ (10) (0x28) -- read blocks from the device.
    Read10 { lba: u32, blocks: u16 },
    /// WRITE (10) (0x2A) -- write blocks to the device.
    Write10 { lba: u32, blocks: u16 },
    /// REQUEST SENSE (0x03) -- get error details after a failure.
    RequestSense,
    /// MODE SENSE (6) (0x1A) -- get device parameters.
    ModeSense6,
}

impl ScsiCommand {
    /// Encode this SCSI command into a CDB (Command Descriptor Block),
    /// padded to 16 bytes as required by the CBW.
    pub fn encode_cdb(&self) -> [u8; 16] {
        let mut cdb = [0u8; 16];
        match self {
            ScsiCommand::TestUnitReady => {
                cdb[0] = 0x00;
                // Bytes 1-5 are zero (6-byte CDB).
            }
            ScsiCommand::Inquiry => {
                cdb[0] = 0x12;
                // Allocation length = 36 bytes (standard INQUIRY).
                cdb[4] = 36;
            }
            ScsiCommand::ReadCapacity10 => {
                cdb[0] = 0x25;
                // 10-byte CDB, remaining bytes zero.
            }
            ScsiCommand::Read10 { lba, blocks } => {
                cdb[0] = 0x28;
                // LBA in bytes 2-5 (big-endian).
                cdb[2..6].copy_from_slice(&lba.to_be_bytes());
                // Transfer length in bytes 7-8 (big-endian).
                cdb[7..9].copy_from_slice(&blocks.to_be_bytes());
            }
            ScsiCommand::Write10 { lba, blocks } => {
                cdb[0] = 0x2A;
                cdb[2..6].copy_from_slice(&lba.to_be_bytes());
                cdb[7..9].copy_from_slice(&blocks.to_be_bytes());
            }
            ScsiCommand::RequestSense => {
                cdb[0] = 0x03;
                // Allocation length = 18 bytes.
                cdb[4] = 18;
            }
            ScsiCommand::ModeSense6 => {
                cdb[0] = 0x1A;
                // Page code = 0x3F (all pages).
                cdb[2] = 0x3F;
                // Allocation length = 192 bytes.
                cdb[4] = 192;
            }
        }
        cdb
    }

    /// Return the CDB length (number of meaningful bytes before padding).
    pub fn cdb_length(&self) -> u8 {
        match self {
            ScsiCommand::TestUnitReady => 6,
            ScsiCommand::Inquiry => 6,
            ScsiCommand::ReadCapacity10 => 10,
            ScsiCommand::Read10 { .. } => 10,
            ScsiCommand::Write10 { .. } => 10,
            ScsiCommand::RequestSense => 6,
            ScsiCommand::ModeSense6 => 6,
        }
    }
}

// ── CBW / CSW ────────────────────────────────────────────────────────

/// Command Block Wrapper (31 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cbw {
    /// Signature: must be CBW_SIGNATURE (0x43425355).
    pub signature: u32,
    /// Tag: unique ID to match with CSW.
    pub tag: u32,
    /// Expected number of data bytes to transfer.
    pub data_transfer_length: u32,
    /// Flags: 0x00 = data OUT, 0x80 = data IN.
    pub flags: u8,
    /// Logical unit number (usually 0).
    pub lun: u8,
    /// Length of the SCSI command block (1-16).
    pub cb_length: u8,
    /// The SCSI command bytes, padded to 16.
    pub cb: [u8; 16],
}

impl Cbw {
    /// Serialize this CBW into a 31-byte buffer (little-endian).
    pub fn to_bytes(&self) -> [u8; CBW_SIZE] {
        let mut buf = [0u8; CBW_SIZE];
        buf[0..4].copy_from_slice(&self.signature.to_le_bytes());
        buf[4..8].copy_from_slice(&self.tag.to_le_bytes());
        buf[8..12].copy_from_slice(&self.data_transfer_length.to_le_bytes());
        buf[12] = self.flags;
        buf[13] = self.lun;
        buf[14] = self.cb_length;
        buf[15..31].copy_from_slice(&self.cb);
        buf
    }

    /// Deserialize a CBW from a 31-byte buffer.
    pub fn from_bytes(buf: &[u8; CBW_SIZE]) -> Result<Self, ClientError> {
        let signature = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if signature != CBW_SIGNATURE {
            return Err(ClientError::MassStorage(format!(
                "invalid CBW signature: 0x{signature:08X}"
            )));
        }
        Ok(Cbw {
            signature,
            tag: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            data_transfer_length: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            flags: buf[12],
            lun: buf[13],
            cb_length: buf[14],
            cb: {
                let mut cb = [0u8; 16];
                cb.copy_from_slice(&buf[15..31]);
                cb
            },
        })
    }
}

/// Command Status Wrapper (13 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Csw {
    /// Signature: must be CSW_SIGNATURE (0x53425355).
    pub signature: u32,
    /// Tag: must match the CBW tag.
    pub tag: u32,
    /// Number of bytes NOT transferred.
    pub data_residue: u32,
    /// Status: 0=passed, 1=failed, 2=phase error.
    pub status: CswStatus,
}

impl Csw {
    /// Serialize this CSW into a 13-byte buffer (little-endian).
    pub fn to_bytes(&self) -> [u8; CSW_SIZE] {
        let mut buf = [0u8; CSW_SIZE];
        buf[0..4].copy_from_slice(&self.signature.to_le_bytes());
        buf[4..8].copy_from_slice(&self.tag.to_le_bytes());
        buf[8..12].copy_from_slice(&self.data_residue.to_le_bytes());
        buf[12] = self.status.to_byte();
        buf
    }

    /// Deserialize a CSW from a 13-byte buffer.
    pub fn from_bytes(buf: &[u8; CSW_SIZE]) -> Result<Self, ClientError> {
        let signature = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if signature != CSW_SIGNATURE {
            return Err(ClientError::MassStorage(format!(
                "invalid CSW signature: 0x{signature:08X}"
            )));
        }
        Ok(Csw {
            signature,
            tag: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            data_residue: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            status: CswStatus::from_byte(buf[12]),
        })
    }
}

// ── MassStorageDevice ────────────────────────────────────────────────

/// A USB Mass Storage device accessed over USB/IP.
///
/// Wraps a TCP connection to a USB/IP server and provides block-level
/// read/write operations using the Bulk-Only Transport (BBB) protocol.
pub struct MassStorageDevice {
    /// Read half of the TCP stream (after OP_REQ_IMPORT).
    reader: OwnedReadHalf,
    /// Write half of the TCP stream.
    writer: OwnedWriteHalf,
    /// Device ID from the IMPORT response.
    devid: u32,
    /// Bulk OUT endpoint (typically 0x02).
    ep_out: u8,
    /// Bulk IN endpoint (typically 0x81).
    ep_in: u8,
    /// Next sequence number for USB/IP URBs.
    next_seqnum: u32,
    /// Next CBW tag.
    next_tag: u32,
    /// Block size in bytes (from READ CAPACITY, typically 512).
    block_size: u32,
    /// Total number of blocks (from READ CAPACITY).
    total_blocks: u64,
    /// Vendor string from INQUIRY.
    vendor: String,
    /// Product string from INQUIRY.
    product: String,
}

impl MassStorageDevice {
    /// Connect to a USB/IP server and import a mass storage device.
    ///
    /// Sends OP_REQ_IMPORT for the given `busid` and, on success,
    /// returns a `MassStorageDevice` ready for `initialize()`.
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
        let (devid, _device) = match reply {
            OpMessage::RepImport(rep) => {
                if rep.status != 0 {
                    return Err(ClientError::ImportRejected {
                        busid: busid.to_string(),
                        status: rep.status,
                    });
                }
                let device = rep.device.ok_or(ClientError::ImportMissingDevice)?;
                // devid = busnum << 16 | devnum
                let devid = (device.busnum << 16) | device.devnum;
                (devid, device)
            }
            _ => {
                return Err(ClientError::Protocol(
                    extender_protocol::ProtocolError::InvalidOpCode(0),
                ));
            }
        };

        Ok(MassStorageDevice {
            reader,
            writer,
            devid,
            ep_out: DEFAULT_EP_OUT,
            ep_in: DEFAULT_EP_IN,
            next_seqnum: 1,
            next_tag: 1,
            block_size: 0,
            total_blocks: 0,
            vendor: String::new(),
            product: String::new(),
        })
    }

    /// Initialize the device by sending INQUIRY and READ CAPACITY commands.
    ///
    /// Populates vendor/product strings, block size, and total block count.
    pub async fn initialize(&mut self) -> Result<(), ClientError> {
        // INQUIRY
        let inquiry_data = self.scsi_command(&ScsiCommand::Inquiry, None, 36).await?;

        if inquiry_data.len() >= 36 {
            // Vendor: bytes 8-15, Product: bytes 16-31
            self.vendor = String::from_utf8_lossy(&inquiry_data[8..16])
                .trim()
                .to_string();
            self.product = String::from_utf8_lossy(&inquiry_data[16..32])
                .trim()
                .to_string();
        }

        // READ CAPACITY (10)
        let cap_data = self
            .scsi_command(&ScsiCommand::ReadCapacity10, None, 8)
            .await?;

        if cap_data.len() >= 8 {
            let last_lba = u32::from_be_bytes([cap_data[0], cap_data[1], cap_data[2], cap_data[3]]);
            self.block_size =
                u32::from_be_bytes([cap_data[4], cap_data[5], cap_data[6], cap_data[7]]);
            self.total_blocks = last_lba as u64 + 1;
        }

        Ok(())
    }

    /// Read blocks from the device into `buf`.
    ///
    /// Returns the number of bytes actually read.
    pub async fn read_blocks(
        &mut self,
        lba: u64,
        count: u32,
        buf: &mut [u8],
    ) -> Result<usize, ClientError> {
        let expected_len = count as usize * self.block_size as usize;
        if buf.len() < expected_len {
            return Err(ClientError::MassStorage(format!(
                "buffer too small: need {expected_len} bytes, got {}",
                buf.len()
            )));
        }

        let cmd = ScsiCommand::Read10 {
            lba: lba as u32,
            blocks: count as u16,
        };

        let data = self.scsi_command(&cmd, None, expected_len as u32).await?;

        let copy_len = data.len().min(buf.len());
        buf[..copy_len].copy_from_slice(&data[..copy_len]);
        Ok(copy_len)
    }

    /// Write blocks to the device from `data`.
    ///
    /// Returns the number of bytes actually written.
    pub async fn write_blocks(
        &mut self,
        lba: u64,
        count: u32,
        data: &[u8],
    ) -> Result<usize, ClientError> {
        let expected_len = count as usize * self.block_size as usize;
        if data.len() < expected_len {
            return Err(ClientError::MassStorage(format!(
                "data too small: need {expected_len} bytes, got {}",
                data.len()
            )));
        }

        let cmd = ScsiCommand::Write10 {
            lba: lba as u32,
            blocks: count as u16,
        };

        let write_data = &data[..expected_len];
        self.scsi_command(&cmd, Some(write_data), 0).await?;

        Ok(expected_len)
    }

    /// Get the total disk size in bytes.
    pub fn disk_size(&self) -> u64 {
        self.total_blocks * self.block_size as u64
    }

    /// Get the block size in bytes.
    pub fn block_size(&self) -> u32 {
        self.block_size
    }

    /// Get the vendor string from INQUIRY.
    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    /// Get the product string from INQUIRY.
    pub fn product(&self) -> &str {
        &self.product
    }

    /// Get the total number of blocks.
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
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

    /// Allocate the next CBW tag.
    fn next_tag(&mut self) -> u32 {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        if self.next_tag == 0 {
            self.next_tag = 1;
        }
        tag
    }

    /// Build and send a CBW as a USB/IP CmdSubmit (bulk OUT).
    ///
    /// Returns the tag used in the CBW.
    async fn send_cbw(
        &mut self,
        command: &ScsiCommand,
        data_len: u32,
        direction: Direction,
    ) -> Result<u32, ClientError> {
        let tag = self.next_tag();
        let cbw = Cbw {
            signature: CBW_SIGNATURE,
            tag,
            data_transfer_length: data_len,
            flags: if direction == Direction::In {
                0x80
            } else {
                0x00
            },
            lun: 0,
            cb_length: command.cdb_length(),
            cb: command.encode_cdb(),
        };

        let cbw_bytes = cbw.to_bytes();
        self.send_bulk_out(&cbw_bytes).await?;
        Ok(tag)
    }

    /// Receive and parse a CSW from a USB/IP RetSubmit (bulk IN).
    async fn receive_csw(&mut self, expected_tag: u32) -> Result<CswStatus, ClientError> {
        let data = self.receive_bulk_in(CSW_SIZE as u32).await?;

        if data.len() < CSW_SIZE {
            return Err(ClientError::MassStorage(format!(
                "CSW too short: expected {CSW_SIZE} bytes, got {}",
                data.len()
            )));
        }

        let mut csw_buf = [0u8; CSW_SIZE];
        csw_buf.copy_from_slice(&data[..CSW_SIZE]);
        let csw = Csw::from_bytes(&csw_buf)?;

        if csw.tag != expected_tag {
            return Err(ClientError::MassStorage(format!(
                "CSW tag mismatch: expected 0x{expected_tag:08X}, got 0x{:08X}",
                csw.tag
            )));
        }

        Ok(csw.status)
    }

    /// Execute a full SCSI command (CBW -> optional data -> CSW).
    ///
    /// For data-in commands (e.g., READ), `response_len` specifies the
    /// expected response size and `data` should be `None`.
    ///
    /// For data-out commands (e.g., WRITE), `data` contains the payload
    /// and `response_len` should be 0.
    async fn scsi_command(
        &mut self,
        cmd: &ScsiCommand,
        data: Option<&[u8]>,
        response_len: u32,
    ) -> Result<Vec<u8>, ClientError> {
        let (direction, data_len) = if let Some(d) = data {
            (Direction::Out, d.len() as u32)
        } else if response_len > 0 {
            (Direction::In, response_len)
        } else {
            (Direction::Out, 0)
        };

        // Phase 1: Send CBW
        let tag = self.send_cbw(cmd, data_len, direction).await?;

        // Wait for CBW RetSubmit
        self.read_ret_submit().await?;

        // Phase 2: Data transfer
        let response_data = if let Some(d) = data {
            // Data OUT: send data to device
            self.send_bulk_out(d).await?;
            self.read_ret_submit().await?;
            Vec::new()
        } else if response_len > 0 {
            // Data IN: receive data from device
            self.receive_bulk_in(response_len).await?
        } else {
            Vec::new()
        };

        // Phase 3: Receive CSW
        let status = self.receive_csw(tag).await?;

        match status {
            CswStatus::Passed => Ok(response_data),
            CswStatus::Failed => Err(ClientError::MassStorage(
                "SCSI command failed (CSW status=failed)".to_string(),
            )),
            CswStatus::PhaseError => Err(ClientError::MassStorage(
                "SCSI phase error (device needs reset)".to_string(),
            )),
            CswStatus::Unknown(b) => Err(ClientError::MassStorage(format!(
                "unknown CSW status: 0x{b:02X}"
            ))),
        }
    }

    /// Send a bulk OUT transfer via USB/IP (CmdSubmit + wait for RetSubmit).
    async fn send_bulk_out(&mut self, payload: &[u8]) -> Result<(), ClientError> {
        let seqnum = self.next_seqnum();
        let ep = self.ep_out & 0x0F; // strip direction bit

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
        };

        let mut buf = BytesMut::new();
        cmd.encode(&mut buf);
        self.writer.write_all(&buf).await.map_err(ClientError::Io)?;

        Ok(())
    }

    /// Send a bulk IN request and read the response data from RetSubmit.
    async fn receive_bulk_in(&mut self, length: u32) -> Result<Vec<u8>, ClientError> {
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
        };

        let mut buf = BytesMut::new();
        cmd.encode(&mut buf);
        self.writer.write_all(&buf).await.map_err(ClientError::Io)?;

        // Read the RetSubmit response
        let ret = self.read_ret_submit().await?;
        Ok(ret)
    }

    /// Read a RetSubmit message from the USB/IP stream, returning the transfer buffer.
    async fn read_ret_submit(&mut self) -> Result<Vec<u8>, ClientError> {
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::MassStorage(format!(
                        "URB transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(ret.transfer_buffer.to_vec())
            }
            other => Err(ClientError::MassStorage(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CBW serialization ────────────────────────────────────────────

    #[test]
    fn test_cbw_signature() {
        let cbw = Cbw {
            signature: CBW_SIGNATURE,
            tag: 1,
            data_transfer_length: 0,
            flags: 0x00,
            lun: 0,
            cb_length: 6,
            cb: [0u8; 16],
        };
        let bytes = cbw.to_bytes();
        // "USBC" in little-endian: 0x55, 0x53, 0x42, 0x43
        assert_eq!(&bytes[0..4], &[0x55, 0x53, 0x42, 0x43]);
    }

    #[test]
    fn test_cbw_size() {
        let cbw = Cbw {
            signature: CBW_SIGNATURE,
            tag: 0xAABBCCDD,
            data_transfer_length: 512,
            flags: 0x80,
            lun: 0,
            cb_length: 10,
            cb: [0u8; 16],
        };
        let bytes = cbw.to_bytes();
        assert_eq!(bytes.len(), 31);
    }

    #[test]
    fn test_cbw_roundtrip() {
        let cbw = Cbw {
            signature: CBW_SIGNATURE,
            tag: 42,
            data_transfer_length: 1024,
            flags: 0x80,
            lun: 0,
            cb_length: 10,
            cb: {
                let mut cb = [0u8; 16];
                cb[0] = 0x28; // READ(10)
                cb[2] = 0x00;
                cb[3] = 0x00;
                cb[4] = 0x00;
                cb[5] = 0x08;
                cb[7] = 0x00;
                cb[8] = 0x02;
                cb
            },
        };
        let bytes = cbw.to_bytes();
        let decoded = Cbw::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, cbw);
    }

    #[test]
    fn test_cbw_invalid_signature() {
        let mut bytes = [0u8; 31];
        // Wrong signature
        bytes[0..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        let result = Cbw::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_cbw_fields() {
        let cbw = Cbw {
            signature: CBW_SIGNATURE,
            tag: 0x0102_0304,
            data_transfer_length: 0x0506_0708,
            flags: 0x80,
            lun: 2,
            cb_length: 10,
            cb: [0xAA; 16],
        };
        let bytes = cbw.to_bytes();

        // Tag (little-endian)
        assert_eq!(&bytes[4..8], &[0x04, 0x03, 0x02, 0x01]);
        // Data transfer length (little-endian)
        assert_eq!(&bytes[8..12], &[0x08, 0x07, 0x06, 0x05]);
        // Flags
        assert_eq!(bytes[12], 0x80);
        // LUN
        assert_eq!(bytes[13], 2);
        // CB length
        assert_eq!(bytes[14], 10);
        // CB bytes
        assert_eq!(&bytes[15..31], &[0xAA; 16]);
    }

    // ── CSW serialization ────────────────────────────────────────────

    #[test]
    fn test_csw_signature() {
        let csw = Csw {
            signature: CSW_SIGNATURE,
            tag: 1,
            data_residue: 0,
            status: CswStatus::Passed,
        };
        let bytes = csw.to_bytes();
        // "USBS" in little-endian: 0x55, 0x53, 0x42, 0x53
        assert_eq!(&bytes[0..4], &[0x55, 0x53, 0x42, 0x53]);
    }

    #[test]
    fn test_csw_size() {
        let csw = Csw {
            signature: CSW_SIGNATURE,
            tag: 1,
            data_residue: 0,
            status: CswStatus::Passed,
        };
        let bytes = csw.to_bytes();
        assert_eq!(bytes.len(), 13);
    }

    #[test]
    fn test_csw_roundtrip() {
        let csw = Csw {
            signature: CSW_SIGNATURE,
            tag: 42,
            data_residue: 100,
            status: CswStatus::Failed,
        };
        let bytes = csw.to_bytes();
        let decoded = Csw::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, csw);
    }

    #[test]
    fn test_csw_status_values() {
        assert_eq!(CswStatus::from_byte(0), CswStatus::Passed);
        assert_eq!(CswStatus::from_byte(1), CswStatus::Failed);
        assert_eq!(CswStatus::from_byte(2), CswStatus::PhaseError);
        assert_eq!(CswStatus::from_byte(3), CswStatus::Unknown(3));

        assert_eq!(CswStatus::Passed.to_byte(), 0);
        assert_eq!(CswStatus::Failed.to_byte(), 1);
        assert_eq!(CswStatus::PhaseError.to_byte(), 2);
        assert_eq!(CswStatus::Unknown(0xFF).to_byte(), 0xFF);
    }

    #[test]
    fn test_csw_invalid_signature() {
        let mut bytes = [0u8; 13];
        bytes[0..4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        let result = Csw::from_bytes(&bytes);
        assert!(result.is_err());
    }

    // ── SCSI CDB encoding ───────────────────────────────────────────

    #[test]
    fn test_test_unit_ready_cdb() {
        let cmd = ScsiCommand::TestUnitReady;
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x00);
        assert_eq!(cmd.cdb_length(), 6);
        // All other bytes should be zero.
        assert!(cdb[1..16].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_inquiry_cdb() {
        let cmd = ScsiCommand::Inquiry;
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x12);
        assert_eq!(cdb[4], 36); // allocation length
        assert_eq!(cmd.cdb_length(), 6);
    }

    #[test]
    fn test_read_capacity10_cdb() {
        let cmd = ScsiCommand::ReadCapacity10;
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x25);
        assert_eq!(cmd.cdb_length(), 10);
    }

    #[test]
    fn test_read10_cdb() {
        let cmd = ScsiCommand::Read10 {
            lba: 0x0000_0100,
            blocks: 8,
        };
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x28);
        // LBA in bytes 2-5 (big-endian)
        assert_eq!(&cdb[2..6], &[0x00, 0x00, 0x01, 0x00]);
        // Transfer length in bytes 7-8 (big-endian)
        assert_eq!(&cdb[7..9], &[0x00, 0x08]);
        assert_eq!(cmd.cdb_length(), 10);
    }

    #[test]
    fn test_write10_cdb() {
        let cmd = ScsiCommand::Write10 {
            lba: 0x0000_0200,
            blocks: 4,
        };
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x2A);
        assert_eq!(&cdb[2..6], &[0x00, 0x00, 0x02, 0x00]);
        assert_eq!(&cdb[7..9], &[0x00, 0x04]);
        assert_eq!(cmd.cdb_length(), 10);
    }

    #[test]
    fn test_request_sense_cdb() {
        let cmd = ScsiCommand::RequestSense;
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x03);
        assert_eq!(cdb[4], 18); // allocation length
        assert_eq!(cmd.cdb_length(), 6);
    }

    #[test]
    fn test_mode_sense6_cdb() {
        let cmd = ScsiCommand::ModeSense6;
        let cdb = cmd.encode_cdb();
        assert_eq!(cdb[0], 0x1A);
        assert_eq!(cdb[2], 0x3F); // all pages
        assert_eq!(cdb[4], 192); // allocation length
        assert_eq!(cmd.cdb_length(), 6);
    }

    #[test]
    fn test_read10_large_lba() {
        let cmd = ScsiCommand::Read10 {
            lba: 0xFFFF_FFFF,
            blocks: 0xFFFF,
        };
        let cdb = cmd.encode_cdb();
        assert_eq!(&cdb[2..6], &[0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(&cdb[7..9], &[0xFF, 0xFF]);
    }

    // ── Integration test with mock server ────────────────────────────

    #[tokio::test]
    async fn test_mock_inquiry_and_read_capacity() {
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

            // 2) Send OP_REP_IMPORT with a device descriptor
            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 2,
                speed: 3,
                id_vendor: 0x0781,
                id_product: 0x5567,
                bcd_device: 0x0100,
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

            // Helper to respond to URBs. We expect the following sequence:
            // INQUIRY: CBW OUT -> RetSubmit, Data IN -> RetSubmit(inquiry data), CSW IN -> RetSubmit(csw)
            // READ CAPACITY: CBW OUT -> RetSubmit, Data IN -> RetSubmit(cap data), CSW IN -> RetSubmit(csw)

            // -- INQUIRY --
            // CBW OUT: read CmdSubmit, reply with RetSubmit (success, no data)
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                                                     // Parse the CBW from transfer_buffer
                let cbw_data = &cmd.transfer_buffer;
                assert_eq!(cbw_data.len(), 31);
                let mut cbw_arr = [0u8; 31];
                cbw_arr.copy_from_slice(cbw_data);
                let cbw = Cbw::from_bytes(&cbw_arr).unwrap();
                assert_eq!(cbw.cb[0], 0x12); // INQUIRY

                // Send RetSubmit for the CBW
                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: 31,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                });
                write_urb_message(&mut writer, &ret).await.unwrap();

                // Data IN: read CmdSubmit (direction=IN), reply with inquiry data
                let urb2 = read_urb_message(&mut reader).await.unwrap();
                if let extender_protocol::UrbMessage::CmdSubmit(cmd2) = &urb2 {
                    assert_eq!(cmd2.header.direction, 1); // IN
                    let mut inquiry_response = vec![0u8; 36];
                    // Peripheral device type 0x00 (direct access)
                    inquiry_response[0] = 0x00;
                    // Vendor: "TestVndr" at offset 8
                    inquiry_response[8..16].copy_from_slice(b"TestVndr");
                    // Product: "TestProduct12345" at offset 16
                    inquiry_response[16..32].copy_from_slice(b"TestProduct12345");

                    let ret2 = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                        header: UsbipHeaderBasic {
                            command: Command::RetSubmit as u32,
                            seqnum: cmd2.header.seqnum,
                            devid: cmd2.header.devid,
                            direction: 1,
                            ep: cmd2.header.ep,
                        },
                        status: 0,
                        actual_length: 36,
                        start_frame: 0,
                        number_of_packets: 0,
                        error_count: 0,
                        transfer_buffer: Bytes::from(inquiry_response),
                    });
                    write_urb_message(&mut writer, &ret2).await.unwrap();
                }

                // CSW IN: read CmdSubmit, reply with CSW
                let urb3 = read_urb_message(&mut reader).await.unwrap();
                if let extender_protocol::UrbMessage::CmdSubmit(cmd3) = &urb3 {
                    assert_eq!(cmd3.header.direction, 1); // IN
                    let csw = Csw {
                        signature: CSW_SIGNATURE,
                        tag: cbw.tag,
                        data_residue: 0,
                        status: CswStatus::Passed,
                    };
                    let ret3 = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                        header: UsbipHeaderBasic {
                            command: Command::RetSubmit as u32,
                            seqnum: cmd3.header.seqnum,
                            devid: cmd3.header.devid,
                            direction: 1,
                            ep: cmd3.header.ep,
                        },
                        status: 0,
                        actual_length: 13,
                        start_frame: 0,
                        number_of_packets: 0,
                        error_count: 0,
                        transfer_buffer: Bytes::from(csw.to_bytes().to_vec()),
                    });
                    write_urb_message(&mut writer, &ret3).await.unwrap();
                }
            }

            // -- READ CAPACITY (10) --
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                let cbw_data = &cmd.transfer_buffer;
                let mut cbw_arr = [0u8; 31];
                cbw_arr.copy_from_slice(cbw_data);
                let cbw = Cbw::from_bytes(&cbw_arr).unwrap();
                assert_eq!(cbw.cb[0], 0x25); // READ CAPACITY

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: 31,
                    start_frame: 0,
                    number_of_packets: 0,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                });
                write_urb_message(&mut writer, &ret).await.unwrap();

                // Data IN
                let urb2 = read_urb_message(&mut reader).await.unwrap();
                if let extender_protocol::UrbMessage::CmdSubmit(cmd2) = &urb2 {
                    // Last LBA = 2047 (0x000007FF) => 2048 blocks
                    // Block size = 512 (0x00000200)
                    let mut cap_data = vec![0u8; 8];
                    cap_data[0..4].copy_from_slice(&0x0000_07FFu32.to_be_bytes());
                    cap_data[4..8].copy_from_slice(&0x0000_0200u32.to_be_bytes());

                    let ret2 = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                        header: UsbipHeaderBasic {
                            command: Command::RetSubmit as u32,
                            seqnum: cmd2.header.seqnum,
                            devid: cmd2.header.devid,
                            direction: 1,
                            ep: cmd2.header.ep,
                        },
                        status: 0,
                        actual_length: 8,
                        start_frame: 0,
                        number_of_packets: 0,
                        error_count: 0,
                        transfer_buffer: Bytes::from(cap_data),
                    });
                    write_urb_message(&mut writer, &ret2).await.unwrap();
                }

                // CSW IN
                let urb3 = read_urb_message(&mut reader).await.unwrap();
                if let extender_protocol::UrbMessage::CmdSubmit(cmd3) = &urb3 {
                    let csw = Csw {
                        signature: CSW_SIGNATURE,
                        tag: cbw.tag,
                        data_residue: 0,
                        status: CswStatus::Passed,
                    };
                    let ret3 = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                        header: UsbipHeaderBasic {
                            command: Command::RetSubmit as u32,
                            seqnum: cmd3.header.seqnum,
                            devid: cmd3.header.devid,
                            direction: 1,
                            ep: cmd3.header.ep,
                        },
                        status: 0,
                        actual_length: 13,
                        start_frame: 0,
                        number_of_packets: 0,
                        error_count: 0,
                        transfer_buffer: Bytes::from(csw.to_bytes().to_vec()),
                    });
                    write_urb_message(&mut writer, &ret3).await.unwrap();
                }
            }
        });

        // Client side
        let mut dev = MassStorageDevice::connect(addr, "1-1").await.unwrap();
        dev.initialize().await.unwrap();

        assert_eq!(dev.vendor(), "TestVndr");
        assert_eq!(dev.product(), "TestProduct12345");
        assert_eq!(dev.block_size(), 512);
        assert_eq!(dev.total_blocks(), 2048);
        assert_eq!(dev.disk_size(), 2048 * 512);

        server.await.unwrap();
    }
}
