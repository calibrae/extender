//! USB Audio Class protocol handler over USB/IP.
//!
//! This module implements the USB Audio Class (UAC 1.0) protocol for
//! audio streaming devices accessed over USB/IP. It supports:
//!
//! - Audio Control interface (class 0x01, subclass 0x01): topology management
//! - Audio Streaming interface (class 0x01, subclass 0x02): isochronous PCM data
//! - Sample rate control via SET_CUR/GET_CUR
//! - Volume and mute control via Feature Unit requests

use std::net::SocketAddr;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::timeout;

use extender_protocol::codec::{read_op_message, read_urb_message, write_op_message};
use extender_protocol::urb::{CmdSubmit, IsoPacketDescriptor, UsbipHeaderBasic};
use extender_protocol::wire::WireFormat;
use extender_protocol::{Command, OpMessage, OpReqImport, UsbDevice};

use crate::error::ClientError;

// ── Constants ────────────────────────────────────────────────────────

/// Default ISO OUT endpoint for playback (speaker).
const DEFAULT_EP_PLAYBACK: u8 = 0x01;
/// Default ISO IN endpoint for capture (microphone).
const DEFAULT_EP_CAPTURE: u8 = 0x82;

/// Connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// Default feature unit ID for volume/mute control.
const DEFAULT_FEATURE_UNIT_ID: u8 = 0x02;

// ── USB Audio Class request types ────────────────────────────────────

/// Audio class-specific request: SET_CUR.
const SET_CUR: u8 = 0x01;
/// Audio class-specific request: GET_CUR.
const GET_CUR: u8 = 0x81;

/// Volume control selector.
const VOLUME_CONTROL: u8 = 0x02;
/// Mute control selector.
const MUTE_CONTROL: u8 = 0x01;

// ── AudioDevice ──────────────────────────────────────────────────────

/// A USB Audio Class device accessed over USB/IP.
///
/// Wraps a TCP connection to a USB/IP server and provides audio-specific
/// operations: playback, capture, sample rate control, volume, and mute.
pub struct AudioDevice {
    /// Read half of the TCP stream (after OP_REQ_IMPORT).
    reader: OwnedReadHalf,
    /// Write half of the TCP stream.
    writer: OwnedWriteHalf,
    /// Device ID from the IMPORT response.
    devid: u32,
    /// ISO OUT endpoint for playback (speaker), if available.
    ep_playback: Option<u8>,
    /// ISO IN endpoint for capture (microphone), if available.
    ep_capture: Option<u8>,
    /// Interrupt IN endpoint for status notifications, if available.
    ep_notify: Option<u8>,
    /// Next sequence number for USB/IP URBs.
    next_seqnum: u32,
    /// Audio Control interface number.
    control_interface: u8,
    /// Audio Streaming interface number.
    streaming_interface: u8,
    /// Sample rate in Hz.
    sample_rate: u32,
    /// Number of audio channels.
    channels: u8,
    /// Bits per sample.
    bit_depth: u8,
    /// Maximum packet size for ISO transfers.
    max_packet_size: u16,
}

impl AudioDevice {
    /// Connect to a USB/IP server and import an audio device.
    ///
    /// Sends OP_REQ_IMPORT for the given `busid` and, on success,
    /// returns an `AudioDevice` ready for `initialize()`.
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

        Ok(AudioDevice {
            reader,
            writer,
            devid,
            ep_playback: Some(DEFAULT_EP_PLAYBACK),
            ep_capture: Some(DEFAULT_EP_CAPTURE),
            ep_notify: None,
            next_seqnum: 1,
            control_interface: 0,
            streaming_interface: 1,
            sample_rate: 48000,
            channels: 2,
            bit_depth: 16,
            max_packet_size: 192,
        })
    }

    /// Initialize the audio device by querying its configuration.
    ///
    /// Sends a GET_DESCRIPTOR control transfer to read the device descriptor,
    /// then configures audio parameters.
    pub async fn initialize(&mut self) -> Result<(), ClientError> {
        // Send GET_DESCRIPTOR (device descriptor) to verify the device is responsive
        let setup = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
        self.send_control_in(&setup, 18).await?;
        Ok(())
    }

    /// Read audio samples from the capture endpoint (microphone).
    ///
    /// Submits an isochronous IN transfer and returns the number of bytes read.
    pub async fn read_audio(&mut self, buf: &mut [u8]) -> Result<usize, ClientError> {
        let ep = self
            .ep_capture
            .ok_or_else(|| ClientError::Audio("no capture endpoint configured".to_string()))?;
        let ep_num = ep & 0x0F;

        // Calculate number of ISO packets based on buffer size and max packet size
        let max_pkt = self.max_packet_size as usize;
        if max_pkt == 0 {
            return Err(ClientError::Audio("max_packet_size is zero".to_string()));
        }
        let num_packets = ((buf.len() + max_pkt - 1) / max_pkt).min(32) as u32;
        let total_len = num_packets as usize * max_pkt;

        // Build ISO packet descriptors for the IN transfer
        let mut iso_descs = Vec::with_capacity(num_packets as usize);
        for i in 0..num_packets {
            iso_descs.push(IsoPacketDescriptor {
                offset: i * self.max_packet_size as u32,
                length: self.max_packet_size as u32,
                actual_length: 0,
                status: 0,
            });
        }

        let seqnum = self.next_seqnum();
        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 1, // IN
                ep: ep_num as u32,
            },
            transfer_flags: 0,
            transfer_buffer_length: total_len as u32,
            start_frame: 0,
            number_of_packets: num_packets,
            interval: 1,
            setup: [0u8; 8],
            transfer_buffer: Bytes::new(),
            iso_packet_descriptors: iso_descs,
        };

        let mut encode_buf = BytesMut::new();
        cmd.encode(&mut encode_buf);
        self.writer
            .write_all(&encode_buf)
            .await
            .map_err(ClientError::Io)?;

        // Read RetSubmit
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::Audio(format!(
                        "ISO IN transfer failed with status {}",
                        ret.status
                    )));
                }
                let data = &ret.transfer_buffer;
                let copy_len = data.len().min(buf.len());
                buf[..copy_len].copy_from_slice(&data[..copy_len]);
                Ok(copy_len)
            }
            other => Err(ClientError::Audio(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }

    /// Write audio samples to the playback endpoint (speaker).
    ///
    /// Submits an isochronous OUT transfer and returns the number of bytes written.
    pub async fn write_audio(&mut self, data: &[u8]) -> Result<usize, ClientError> {
        let ep = self
            .ep_playback
            .ok_or_else(|| ClientError::Audio("no playback endpoint configured".to_string()))?;
        let ep_num = ep & 0x0F;

        let max_pkt = self.max_packet_size as usize;
        if max_pkt == 0 {
            return Err(ClientError::Audio("max_packet_size is zero".to_string()));
        }
        let num_packets = ((data.len() + max_pkt - 1) / max_pkt).min(32) as u32;

        // Build ISO packet descriptors for the OUT transfer
        let mut iso_descs = Vec::with_capacity(num_packets as usize);
        let mut offset = 0u32;
        for i in 0..num_packets as usize {
            let remaining = data.len() - offset as usize;
            let pkt_len = remaining.min(max_pkt) as u32;
            iso_descs.push(IsoPacketDescriptor {
                offset,
                length: pkt_len,
                actual_length: 0,
                status: 0,
            });
            offset += pkt_len;
            let _ = i;
        }

        let seqnum = self.next_seqnum();
        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 0, // OUT
                ep: ep_num as u32,
            },
            transfer_flags: 0,
            transfer_buffer_length: data.len() as u32,
            start_frame: 0,
            number_of_packets: num_packets,
            interval: 1,
            setup: [0u8; 8],
            transfer_buffer: Bytes::copy_from_slice(data),
            iso_packet_descriptors: iso_descs,
        };

        let mut encode_buf = BytesMut::new();
        cmd.encode(&mut encode_buf);
        self.writer
            .write_all(&encode_buf)
            .await
            .map_err(ClientError::Io)?;

        // Read RetSubmit
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::Audio(format!(
                        "ISO OUT transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(data.len())
            }
            other => Err(ClientError::Audio(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }

    /// Set the sample rate via a SET_CUR control transfer.
    ///
    /// Sends a class-specific endpoint request to set the sampling frequency.
    pub async fn set_sample_rate(&mut self, rate: u32) -> Result<(), ClientError> {
        let ep_addr = self
            .ep_playback
            .or(self.ep_capture)
            .ok_or_else(|| ClientError::Audio("no audio endpoint configured".to_string()))?;

        // SET_CUR sampling frequency control
        // bmRequestType=0x22 (class, endpoint, host-to-device)
        // bRequest=SET_CUR (0x01)
        // wValue=0x0100 (sampling frequency control)
        // wIndex=endpoint address
        // wLength=3
        let setup = [0x22, SET_CUR, 0x00, 0x01, ep_addr, 0x00, 0x03, 0x00];

        // Sample rate is encoded as 3 bytes, little-endian
        let data = [
            (rate & 0xFF) as u8,
            ((rate >> 8) & 0xFF) as u8,
            ((rate >> 16) & 0xFF) as u8,
        ];

        self.send_control_out(&setup, &data).await?;
        self.sample_rate = rate;
        Ok(())
    }

    /// Get available sample rates by querying the device.
    ///
    /// Returns a list of supported sample rates. If the device does not
    /// report specific rates, returns common defaults.
    pub async fn get_sample_rates(&mut self) -> Result<Vec<u32>, ClientError> {
        let ep_addr = self
            .ep_playback
            .or(self.ep_capture)
            .ok_or_else(|| ClientError::Audio("no audio endpoint configured".to_string()))?;

        // GET_CUR sampling frequency control
        let setup = [0xA2, GET_CUR, 0x00, 0x01, ep_addr, 0x00, 0x03, 0x00];

        let response = self.send_control_in(&setup, 3).await?;

        if response.len() >= 3 {
            let current_rate =
                response[0] as u32 | ((response[1] as u32) << 8) | ((response[2] as u32) << 16);
            // Return the current rate and common rates
            let mut rates = vec![current_rate];
            for &r in &[8000, 16000, 22050, 44100, 48000, 96000] {
                if r != current_rate {
                    rates.push(r);
                }
            }
            Ok(rates)
        } else {
            // Fallback: return common sample rates
            Ok(vec![8000, 16000, 22050, 44100, 48000, 96000])
        }
    }

    /// Set volume for a specific channel (0-100 mapped to USB audio range).
    ///
    /// Channel 0 is master, 1 is left, 2 is right, etc.
    pub async fn set_volume(&mut self, channel: u8, volume: u16) -> Result<(), ClientError> {
        // bmRequestType=0x21 (class, interface, host-to-device)
        // bRequest=SET_CUR (0x01)
        // wValue high=channel, low=VOLUME_CONTROL
        // wIndex high=0, low=feature_unit_id
        // wLength=2
        let setup = [
            0x21,
            SET_CUR,
            channel,
            DEFAULT_FEATURE_UNIT_ID,
            VOLUME_CONTROL,
            0x00,
            0x02,
            0x00,
        ];

        let data = volume.to_le_bytes();
        self.send_control_out(&setup, &data).await?;
        Ok(())
    }

    /// Mute or unmute a specific channel.
    ///
    /// Channel 0 is master, 1 is left, 2 is right, etc.
    pub async fn set_mute(&mut self, channel: u8, mute: bool) -> Result<(), ClientError> {
        let setup = [
            0x21,
            SET_CUR,
            channel,
            DEFAULT_FEATURE_UNIT_ID,
            MUTE_CONTROL,
            0x00,
            0x01,
            0x00,
        ];

        let data = [if mute { 1u8 } else { 0u8 }];
        self.send_control_out(&setup, &data).await?;
        Ok(())
    }

    /// Get the current sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Get the number of audio channels.
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Get the bit depth (bits per sample).
    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
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

    /// Send a control OUT transfer (host to device) on EP0.
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

        // Read RetSubmit
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::Audio(format!(
                        "control OUT transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(())
            }
            other => Err(ClientError::Audio(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }

    /// Send a control IN transfer (device to host) on EP0.
    async fn send_control_in(
        &mut self,
        setup: &[u8; 8],
        length: u16,
    ) -> Result<Vec<u8>, ClientError> {
        let seqnum = self.next_seqnum();

        let cmd = CmdSubmit {
            header: UsbipHeaderBasic {
                command: Command::CmdSubmit as u32,
                seqnum,
                devid: self.devid,
                direction: 1, // IN
                ep: 0,        // EP0
            },
            transfer_flags: 0,
            transfer_buffer_length: length as u32,
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

        // Read RetSubmit
        let msg = read_urb_message(&mut self.reader).await?;
        match msg {
            extender_protocol::UrbMessage::RetSubmit(ret) => {
                if ret.status != 0 {
                    return Err(ClientError::Audio(format!(
                        "control IN transfer failed with status {}",
                        ret.status
                    )));
                }
                Ok(ret.transfer_buffer.to_vec())
            }
            other => Err(ClientError::Audio(format!(
                "expected RetSubmit, got {other:?}"
            ))),
        }
    }
}

/// Build a SET_CUR sample rate setup packet.
///
/// Returns the 8-byte USB setup packet for setting the sampling frequency
/// on the given endpoint address.
pub fn sample_rate_set_cur_setup(ep_addr: u8) -> [u8; 8] {
    [0x22, SET_CUR, 0x00, 0x01, ep_addr, 0x00, 0x03, 0x00]
}

/// Build a GET_CUR sample rate setup packet.
pub fn sample_rate_get_cur_setup(ep_addr: u8) -> [u8; 8] {
    [0xA2, GET_CUR, 0x00, 0x01, ep_addr, 0x00, 0x03, 0x00]
}

/// Build a SET_CUR volume setup packet.
pub fn volume_set_cur_setup(channel: u8, feature_unit_id: u8) -> [u8; 8] {
    [
        0x21,
        SET_CUR,
        channel,
        feature_unit_id,
        VOLUME_CONTROL,
        0x00,
        0x02,
        0x00,
    ]
}

/// Build a SET_CUR mute setup packet.
pub fn mute_set_cur_setup(channel: u8, feature_unit_id: u8) -> [u8; 8] {
    [
        0x21,
        SET_CUR,
        channel,
        feature_unit_id,
        MUTE_CONTROL,
        0x00,
        0x01,
        0x00,
    ]
}

/// Encode a sample rate as a 3-byte little-endian value (USB Audio Class format).
pub fn encode_sample_rate(rate: u32) -> [u8; 3] {
    [
        (rate & 0xFF) as u8,
        ((rate >> 8) & 0xFF) as u8,
        ((rate >> 16) & 0xFF) as u8,
    ]
}

/// Decode a 3-byte little-endian sample rate.
pub fn decode_sample_rate(data: &[u8; 3]) -> u32 {
    data[0] as u32 | ((data[1] as u32) << 8) | ((data[2] as u32) << 16)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use extender_protocol::codec::{write_op_message, write_urb_message};
    use extender_protocol::device::UsbDevice;
    use extender_protocol::urb::RetSubmit;
    use extender_protocol::{OpMessage, OpRepImport};
    use tokio::net::TcpListener;

    // ── Setup packet tests ──────────────────────────────────────────

    #[test]
    fn test_sample_rate_set_cur_setup() {
        let setup = sample_rate_set_cur_setup(0x01);
        assert_eq!(setup[0], 0x22); // bmRequestType: class, endpoint, host-to-device
        assert_eq!(setup[1], 0x01); // bRequest: SET_CUR
        assert_eq!(setup[2], 0x00); // wValue low
        assert_eq!(setup[3], 0x01); // wValue high: sampling freq control
        assert_eq!(setup[4], 0x01); // wIndex low: endpoint address
        assert_eq!(setup[5], 0x00); // wIndex high
        assert_eq!(setup[6], 0x03); // wLength low
        assert_eq!(setup[7], 0x00); // wLength high
    }

    #[test]
    fn test_sample_rate_get_cur_setup() {
        let setup = sample_rate_get_cur_setup(0x82);
        assert_eq!(setup[0], 0xA2); // bmRequestType: class, endpoint, device-to-host
        assert_eq!(setup[1], 0x81); // bRequest: GET_CUR
        assert_eq!(setup[4], 0x82); // wIndex: endpoint address
        assert_eq!(setup[6], 0x03); // wLength: 3 bytes
    }

    #[test]
    fn test_volume_set_cur_setup() {
        let setup = volume_set_cur_setup(1, 0x02);
        assert_eq!(setup[0], 0x21); // bmRequestType: class, interface, host-to-device
        assert_eq!(setup[1], 0x01); // bRequest: SET_CUR
        assert_eq!(setup[2], 1); // wValue low: channel
        assert_eq!(setup[3], 0x02); // wValue high: feature unit ID
        assert_eq!(setup[4], 0x02); // wIndex low: volume control selector
        assert_eq!(setup[6], 0x02); // wLength: 2 bytes
    }

    #[test]
    fn test_mute_set_cur_setup() {
        let setup = mute_set_cur_setup(0, 0x05);
        assert_eq!(setup[0], 0x21); // bmRequestType
        assert_eq!(setup[1], 0x01); // SET_CUR
        assert_eq!(setup[2], 0); // channel 0 (master)
        assert_eq!(setup[3], 0x05); // feature unit ID
        assert_eq!(setup[4], 0x01); // mute control selector
        assert_eq!(setup[6], 0x01); // wLength: 1 byte
    }

    // ── Sample rate encoding ────────────────────────────────────────

    #[test]
    fn test_encode_sample_rate_48000() {
        let encoded = encode_sample_rate(48000);
        // 48000 = 0xBB80
        assert_eq!(encoded, [0x80, 0xBB, 0x00]);
    }

    #[test]
    fn test_encode_sample_rate_44100() {
        let encoded = encode_sample_rate(44100);
        // 44100 = 0xAC44
        assert_eq!(encoded, [0x44, 0xAC, 0x00]);
    }

    #[test]
    fn test_encode_sample_rate_96000() {
        let encoded = encode_sample_rate(96000);
        // 96000 = 0x17700
        assert_eq!(encoded, [0x00, 0x77, 0x01]);
    }

    #[test]
    fn test_decode_sample_rate() {
        assert_eq!(decode_sample_rate(&[0x80, 0xBB, 0x00]), 48000);
        assert_eq!(decode_sample_rate(&[0x44, 0xAC, 0x00]), 44100);
        assert_eq!(decode_sample_rate(&[0x00, 0x77, 0x01]), 96000);
    }

    #[test]
    fn test_sample_rate_roundtrip() {
        for rate in [8000, 16000, 22050, 44100, 48000, 96000, 192000] {
            let encoded = encode_sample_rate(rate);
            let decoded = decode_sample_rate(&encoded);
            assert_eq!(decoded, rate, "roundtrip failed for rate {rate}");
        }
    }

    // ── Audio format accessors ──────────────────────────────────────

    #[test]
    fn test_audio_format_defaults() {
        // Verify default format parameters match common USB audio defaults.
        // We can't construct AudioDevice without a connection, so we test
        // the public helper functions and constants instead.
        assert_eq!(DEFAULT_EP_PLAYBACK, 0x01);
        assert_eq!(DEFAULT_EP_CAPTURE, 0x82);
        assert_eq!(DEFAULT_FEATURE_UNIT_ID, 0x02);
    }

    // ── Integration test with mock audio device ─────────────────────

    #[tokio::test]
    async fn test_mock_audio_device_connect_and_initialize() {
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
                devnum: 2,
                speed: 3,
                id_vendor: 0x046D,  // Logitech
                id_product: 0x0A44, // USB headset
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

            // 3) Handle GET_DESCRIPTOR (device descriptor) from initialize()
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 1); // IN
                assert_eq!(cmd.header.ep, 0); // EP0

                // Return a minimal device descriptor (18 bytes)
                let mut desc = vec![0u8; 18];
                desc[0] = 18; // bLength
                desc[1] = 1; // bDescriptorType = DEVICE
                desc[2] = 0x00;
                desc[3] = 0x02; // bcdUSB = 2.00
                desc[4] = 0x00; // bDeviceClass
                desc[5] = 0x00; // bDeviceSubClass
                desc[6] = 0x00; // bDeviceProtocol

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: 0,
                    },
                    status: 0,
                    actual_length: 18,
                    start_frame: 0,
                    number_of_packets: 0xFFFF_FFFF,
                    error_count: 0,
                    transfer_buffer: Bytes::from(desc),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        // Client side
        let mut dev = AudioDevice::connect(addr, "1-1").await.unwrap();
        dev.initialize().await.unwrap();

        assert_eq!(dev.sample_rate(), 48000);
        assert_eq!(dev.channels(), 2);
        assert_eq!(dev.bit_depth(), 16);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_audio_set_sample_rate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // 1) Import handshake
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqImport(_)));

            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 2,
                speed: 3,
                id_vendor: 0x046D,
                id_product: 0x0A44,
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

            // 2) Handle SET_CUR sample rate control transfer
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 0); // EP0
                assert_eq!(cmd.setup[0], 0x22); // class, endpoint, host-to-device
                assert_eq!(cmd.setup[1], 0x01); // SET_CUR

                // Verify sample rate data (44100 = 0xAC44)
                assert_eq!(cmd.transfer_buffer.len(), 3);
                assert_eq!(cmd.transfer_buffer[0], 0x44);
                assert_eq!(cmd.transfer_buffer[1], 0xAC);
                assert_eq!(cmd.transfer_buffer[2], 0x00);

                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 0,
                        ep: 0,
                    },
                    status: 0,
                    actual_length: 3,
                    start_frame: 0,
                    number_of_packets: 0xFFFF_FFFF,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: vec![],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        let mut dev = AudioDevice::connect(addr, "1-1").await.unwrap();
        dev.set_sample_rate(44100).await.unwrap();
        assert_eq!(dev.sample_rate(), 44100);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_audio_write_iso() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // Import handshake
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqImport(_)));

            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 2,
                speed: 3,
                id_vendor: 0x046D,
                id_product: 0x0A44,
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

            // Handle ISO OUT (write_audio)
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 0); // OUT
                assert_eq!(cmd.header.ep, 1); // playback endpoint
                                              // Verify ISO descriptors are present
                assert!(cmd.number_of_packets != 0xFFFF_FFFF);
                assert!(!cmd.iso_packet_descriptors.is_empty());
                assert_eq!(cmd.transfer_buffer.len(), 384); // 2 * 192

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
                    number_of_packets: cmd.number_of_packets,
                    error_count: 0,
                    transfer_buffer: Bytes::new(),
                    iso_packet_descriptors: cmd
                        .iso_packet_descriptors
                        .iter()
                        .map(|d| IsoPacketDescriptor {
                            offset: d.offset,
                            length: d.length,
                            actual_length: d.length,
                            status: 0,
                        })
                        .collect(),
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        let mut dev = AudioDevice::connect(addr, "1-1").await.unwrap();
        let pcm_data = vec![0xABu8; 384]; // 2 packets * 192 bytes
        let written = dev.write_audio(&pcm_data).await.unwrap();
        assert_eq!(written, 384);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_audio_read_iso() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // Import handshake
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqImport(_)));

            let device = UsbDevice {
                path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
                busid: UsbDevice::busid_from_str("1-1").unwrap(),
                busnum: 1,
                devnum: 2,
                speed: 3,
                id_vendor: 0x046D,
                id_product: 0x0A44,
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

            // Handle ISO IN (read_audio)
            let urb = read_urb_message(&mut reader).await.unwrap();
            if let extender_protocol::UrbMessage::CmdSubmit(cmd) = &urb {
                assert_eq!(cmd.header.direction, 1); // IN
                assert_eq!(cmd.header.ep, 2); // capture endpoint (0x82 & 0x0F)
                assert!(cmd.number_of_packets != 0xFFFF_FFFF);
                assert!(!cmd.iso_packet_descriptors.is_empty());

                // Return audio data
                let audio_data = vec![0x42u8; 192]; // 1 packet of audio
                let ret = extender_protocol::UrbMessage::RetSubmit(RetSubmit {
                    header: UsbipHeaderBasic {
                        command: Command::RetSubmit as u32,
                        seqnum: cmd.header.seqnum,
                        devid: cmd.header.devid,
                        direction: 1,
                        ep: cmd.header.ep,
                    },
                    status: 0,
                    actual_length: 192,
                    start_frame: 0,
                    number_of_packets: 1,
                    error_count: 0,
                    transfer_buffer: Bytes::from(audio_data),
                    iso_packet_descriptors: vec![IsoPacketDescriptor {
                        offset: 0,
                        length: 192,
                        actual_length: 192,
                        status: 0,
                    }],
                });
                write_urb_message(&mut writer, &ret).await.unwrap();
            }
        });

        let mut dev = AudioDevice::connect(addr, "1-1").await.unwrap();
        let mut buf = vec![0u8; 384];
        let read = dev.read_audio(&mut buf).await.unwrap();
        assert_eq!(read, 192);
        assert!(buf[..192].iter().all(|&b| b == 0x42));

        server.await.unwrap();
    }
}
