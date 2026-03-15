//! Async protocol codec for reading/writing USB/IP messages over TCP.
//!
//! Provides `read_op_message()` and `read_urb_message()` for the two
//! phases of the protocol, plus corresponding write functions.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::codes::{Command, OpCode, USBIP_VERSION};
use crate::device::USB_DEVICE_WIRE_SIZE;
use crate::discovery::*;
use crate::error::ProtocolError;
use crate::urb::*;
use crate::wire::WireFormat;

/// Read a discovery-phase message from an async reader.
///
/// Reads the 4-byte header (version + opcode) to determine the message type,
/// then reads the remaining bytes accordingly.
pub async fn read_op_message<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<OpMessage, ProtocolError> {
    // Read the common 4-byte prefix: version(2) + opcode(2)
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix).await?;

    let version = u16::from_be_bytes([prefix[0], prefix[1]]);
    if version != USBIP_VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }

    let opcode_raw = u16::from_be_bytes([prefix[2], prefix[3]]);
    let opcode = OpCode::from_raw(opcode_raw).ok_or(ProtocolError::InvalidOpCode(opcode_raw))?;

    match opcode {
        OpCode::OpReqDevlist => {
            // Read remaining 4 bytes (status)
            let mut rest = [0u8; 4];
            reader.read_exact(&mut rest).await?;
            Ok(OpMessage::ReqDevlist(OpReqDevlist))
        }
        OpCode::OpRepDevlist => {
            // Read status(4) + num_devices(4)
            let mut rest = [0u8; 8];
            reader.read_exact(&mut rest).await?;
            let status = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
            let num_devices = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;

            let mut devices = Vec::with_capacity(num_devices);
            for _ in 0..num_devices {
                // Read device descriptor (312 bytes)
                let mut dev_buf = vec![0u8; USB_DEVICE_WIRE_SIZE];
                reader.read_exact(&mut dev_buf).await?;
                let mut cursor = &dev_buf[..];
                let mut device = crate::device::UsbDevice::decode(&mut cursor)?;

                // Read interfaces
                let num_ifaces = device.num_interfaces as usize;
                if num_ifaces > 0 {
                    let iface_bytes = num_ifaces * crate::device::USB_INTERFACE_WIRE_SIZE;
                    let mut iface_buf = vec![0u8; iface_bytes];
                    reader.read_exact(&mut iface_buf).await?;
                    let mut cursor = &iface_buf[..];
                    for _ in 0..num_ifaces {
                        device
                            .interfaces
                            .push(crate::device::UsbInterface::decode(&mut cursor)?);
                    }
                }

                devices.push(device);
            }

            Ok(OpMessage::RepDevlist(OpRepDevlist { status, devices }))
        }
        OpCode::OpReqImport => {
            // Read status(4) + busid(32) = 36 bytes
            let mut rest = [0u8; 36];
            reader.read_exact(&mut rest).await?;
            let mut busid = [0u8; 32];
            busid.copy_from_slice(&rest[4..36]);
            Ok(OpMessage::ReqImport(OpReqImport { busid }))
        }
        OpCode::OpRepImport => {
            // Read status(4)
            let mut status_buf = [0u8; 4];
            reader.read_exact(&mut status_buf).await?;
            let status = u32::from_be_bytes(status_buf);

            if status == 0 {
                // Read device descriptor (312 bytes)
                let mut dev_buf = vec![0u8; USB_DEVICE_WIRE_SIZE];
                reader.read_exact(&mut dev_buf).await?;
                let mut cursor = &dev_buf[..];
                let device = crate::device::UsbDevice::decode(&mut cursor)?;
                Ok(OpMessage::RepImport(Box::new(OpRepImport {
                    status,
                    device: Some(device),
                })))
            } else {
                Ok(OpMessage::RepImport(Box::new(OpRepImport {
                    status,
                    device: None,
                })))
            }
        }
    }
}

/// Write a discovery-phase message to an async writer.
pub async fn write_op_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &OpMessage,
) -> Result<(), ProtocolError> {
    let mut buf = BytesMut::new();
    match msg {
        OpMessage::ReqDevlist(m) => m.encode(&mut buf),
        OpMessage::RepDevlist(m) => m.encode(&mut buf),
        OpMessage::ReqImport(m) => m.encode(&mut buf),
        OpMessage::RepImport(m) => m.encode(&mut buf),
    }
    writer.write_all(&buf).await?;
    Ok(())
}

/// Read a URB-phase message from an async reader.
///
/// Reads the 20-byte basic header to determine the command type,
/// then reads the command-specific fields and any transfer buffer.
pub async fn read_urb_message<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<UrbMessage, ProtocolError> {
    // Read the 20-byte basic header
    let mut header_buf = [0u8; HEADER_BASIC_SIZE];
    reader.read_exact(&mut header_buf).await?;
    let mut cursor = &header_buf[..];
    let header = UsbipHeaderBasic::decode(&mut cursor)?;

    let command =
        Command::from_raw(header.command).ok_or(ProtocolError::InvalidCommand(header.command))?;

    match command {
        Command::CmdSubmit => {
            // Read 28 bytes of submit-specific fields
            let mut fields = [0u8; CMD_SUBMIT_FIELDS_SIZE];
            reader.read_exact(&mut fields).await?;

            let transfer_flags = u32::from_be_bytes([fields[0], fields[1], fields[2], fields[3]]);
            let transfer_buffer_length =
                u32::from_be_bytes([fields[4], fields[5], fields[6], fields[7]]);
            let start_frame = u32::from_be_bytes([fields[8], fields[9], fields[10], fields[11]]);
            let number_of_packets =
                u32::from_be_bytes([fields[12], fields[13], fields[14], fields[15]]);
            let interval = u32::from_be_bytes([fields[16], fields[17], fields[18], fields[19]]);
            let mut setup = [0u8; 8];
            setup.copy_from_slice(&fields[20..28]);

            // For OUT transfers, read the transfer buffer
            let transfer_buffer = if header.direction == 0 && transfer_buffer_length > 0 {
                let mut buf = vec![0u8; transfer_buffer_length as usize];
                reader.read_exact(&mut buf).await?;
                Bytes::from(buf)
            } else {
                Bytes::new()
            };

            Ok(UrbMessage::CmdSubmit(CmdSubmit {
                header,
                transfer_flags,
                transfer_buffer_length,
                start_frame,
                number_of_packets,
                interval,
                setup,
                transfer_buffer,
            }))
        }
        Command::RetSubmit => {
            // Read 28 bytes of return-specific fields
            let mut fields = [0u8; RET_SUBMIT_FIELDS_SIZE];
            reader.read_exact(&mut fields).await?;

            let status = i32::from_be_bytes([fields[0], fields[1], fields[2], fields[3]]);
            let actual_length = u32::from_be_bytes([fields[4], fields[5], fields[6], fields[7]]);
            let start_frame = u32::from_be_bytes([fields[8], fields[9], fields[10], fields[11]]);
            let number_of_packets =
                u32::from_be_bytes([fields[12], fields[13], fields[14], fields[15]]);
            let error_count = u32::from_be_bytes([fields[16], fields[17], fields[18], fields[19]]);
            // fields[20..28] is padding

            // For IN transfers, read the transfer buffer
            let transfer_buffer = if header.direction == 1 && actual_length > 0 {
                let mut buf = vec![0u8; actual_length as usize];
                reader.read_exact(&mut buf).await?;
                Bytes::from(buf)
            } else {
                Bytes::new()
            };

            Ok(UrbMessage::RetSubmit(RetSubmit {
                header,
                status,
                actual_length,
                start_frame,
                number_of_packets,
                error_count,
                transfer_buffer,
            }))
        }
        Command::CmdUnlink => {
            // Read 28 bytes: unlink_seqnum(4) + padding(24)
            let mut fields = [0u8; CMD_UNLINK_FIELDS_SIZE];
            reader.read_exact(&mut fields).await?;

            let unlink_seqnum = u32::from_be_bytes([fields[0], fields[1], fields[2], fields[3]]);

            Ok(UrbMessage::CmdUnlink(CmdUnlink {
                header,
                unlink_seqnum,
            }))
        }
        Command::RetUnlink => {
            // Read 28 bytes: status(4) + padding(24)
            let mut fields = [0u8; RET_UNLINK_FIELDS_SIZE];
            reader.read_exact(&mut fields).await?;

            let status = i32::from_be_bytes([fields[0], fields[1], fields[2], fields[3]]);

            Ok(UrbMessage::RetUnlink(RetUnlink { header, status }))
        }
    }
}

/// Write a URB-phase message to an async writer.
pub async fn write_urb_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &UrbMessage,
) -> Result<(), ProtocolError> {
    let mut buf = BytesMut::new();
    match msg {
        UrbMessage::CmdSubmit(m) => m.encode(&mut buf),
        UrbMessage::RetSubmit(m) => m.encode(&mut buf),
        UrbMessage::CmdUnlink(m) => m.encode(&mut buf),
        UrbMessage::RetUnlink(m) => m.encode(&mut buf),
    }
    writer.write_all(&buf).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{UsbDevice, UsbInterface};

    #[tokio::test]
    async fn test_async_req_devlist_roundtrip() {
        let msg = OpMessage::ReqDevlist(OpReqDevlist);
        let mut buf = Vec::new();
        write_op_message(&mut buf, &msg).await.unwrap();
        assert_eq!(buf.len(), 8);

        let mut cursor = &buf[..];
        let decoded = read_op_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_rep_devlist_roundtrip() {
        let msg = OpMessage::RepDevlist(OpRepDevlist {
            status: 0,
            devices: vec![UsbDevice {
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
            }],
        });

        let mut buf = Vec::new();
        write_op_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_op_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_req_import_roundtrip() {
        let msg = OpMessage::ReqImport(OpReqImport {
            busid: UsbDevice::busid_from_str("1-4.2").unwrap(),
        });
        let mut buf = Vec::new();
        write_op_message(&mut buf, &msg).await.unwrap();
        assert_eq!(buf.len(), 40);

        let mut cursor = &buf[..];
        let decoded = read_op_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_rep_import_success_roundtrip() {
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
        let msg = OpMessage::RepImport(Box::new(OpRepImport {
            status: 0,
            device: Some(device),
        }));

        let mut buf = Vec::new();
        write_op_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_op_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_rep_import_error_roundtrip() {
        let msg = OpMessage::RepImport(Box::new(OpRepImport {
            status: 1,
            device: None,
        }));
        let mut buf = Vec::new();
        write_op_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_op_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_cmd_submit_roundtrip() {
        let msg = UrbMessage::CmdSubmit(CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 0,
                ep: 1,
            },
            transfer_flags: 0,
            transfer_buffer_length: 4,
            start_frame: 0,
            number_of_packets: 0xFFFFFFFF,
            interval: 0,
            setup: [0; 8],
            transfer_buffer: Bytes::from_static(&[0xDE, 0xAD, 0xBE, 0xEF]),
        });

        let mut buf = Vec::new();
        write_urb_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_urb_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_ret_submit_with_data() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let msg = UrbMessage::RetSubmit(RetSubmit {
            header: UsbipHeaderBasic {
                command: Command::RetSubmit as u32,
                seqnum: 1,
                devid: 2,
                direction: 1,
                ep: 0,
            },
            status: 0,
            actual_length: 8,
            start_frame: 0,
            number_of_packets: 0xFFFFFFFF,
            error_count: 0,
            transfer_buffer: Bytes::from(data),
        });

        let mut buf = Vec::new();
        write_urb_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_urb_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_cmd_unlink_roundtrip() {
        let msg = UrbMessage::CmdUnlink(CmdUnlink {
            header: UsbipHeaderBasic {
                command: Command::CmdUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            unlink_seqnum: 3,
        });

        let mut buf = Vec::new();
        write_urb_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_urb_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_ret_unlink_roundtrip() {
        let msg = UrbMessage::RetUnlink(RetUnlink {
            header: UsbipHeaderBasic {
                command: Command::RetUnlink as u32,
                seqnum: 5,
                devid: 2,
                direction: 0,
                ep: 0,
            },
            status: ECONNRESET,
        });

        let mut buf = Vec::new();
        write_urb_message(&mut buf, &msg).await.unwrap();

        let mut cursor = &buf[..];
        let decoded = read_urb_message(&mut cursor).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn test_async_partial_read_simulation() {
        // Simulate reading from a pipe where data arrives in chunks.
        // We use tokio::io::duplex to create a channel pair.
        let (mut client, mut server) = tokio::io::duplex(1024);

        // Spawn a writer that writes byte-by-byte with tiny delays
        let write_handle = tokio::spawn(async move {
            let mut buf = Vec::new();
            OpReqDevlist.encode(&mut buf);
            for byte in &buf {
                client.write_all(std::slice::from_ref(byte)).await.unwrap();
            }
            client
        });

        // Reader should correctly assemble the full message
        let decoded = read_op_message(&mut server).await.unwrap();
        assert_eq!(decoded, OpMessage::ReqDevlist(OpReqDevlist));

        let _client = write_handle.await.unwrap();
    }
}
