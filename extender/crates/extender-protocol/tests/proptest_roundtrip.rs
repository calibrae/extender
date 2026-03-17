//! Property-based round-trip tests for all USB/IP message types.

use bytes::Bytes;
use proptest::prelude::*;

use extender_protocol::*;

// ── Strategies ──────────────────────────────────────────────────────

fn arb_busid() -> impl Strategy<Value = [u8; 32]> {
    // Generate a short ASCII string and pad with zeros
    "[0-9]{1,2}-[0-9]{1,2}(\\.[0-9]{1,2}){0,3}".prop_map(|s| {
        let mut busid = [0u8; 32];
        let len = s.len().min(31);
        busid[..len].copy_from_slice(&s.as_bytes()[..len]);
        busid
    })
}

fn arb_path() -> impl Strategy<Value = [u8; 256]> {
    "/sys/devices/[a-z0-9/_]{1,100}".prop_map(|s| {
        let mut path = [0u8; 256];
        let len = s.len().min(255);
        path[..len].copy_from_slice(&s.as_bytes()[..len]);
        path
    })
}

fn arb_interface() -> impl Strategy<Value = UsbInterface> {
    (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(class, subclass, proto)| UsbInterface {
        interface_class: class,
        interface_subclass: subclass,
        interface_protocol: proto,
        padding: 0,
    })
}

fn arb_device() -> impl Strategy<Value = UsbDevice> {
    // Split into two tuples to stay within proptest's 12-element limit.
    (
        (
            arb_path(),
            arb_busid(),
            any::<u32>(),
            any::<u32>(),
            0u32..6u32, // speed: 0-5
            any::<u16>(),
            any::<u16>(),
            any::<u16>(),
        ),
        (
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            prop::collection::vec(arb_interface(), 0..8),
        ),
    )
        .prop_map(
            |(
                (path, busid, busnum, devnum, speed, id_vendor, id_product, bcd_device),
                (
                    device_class,
                    device_subclass,
                    device_protocol,
                    configuration_value,
                    num_configurations,
                    interfaces,
                ),
            )| {
                UsbDevice {
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
                    num_interfaces: interfaces.len() as u8,
                    interfaces,
                }
            },
        )
}

fn arb_header_basic(command: Command) -> impl Strategy<Value = UsbipHeaderBasic> {
    (any::<u32>(), any::<u32>(), 0u32..2u32, any::<u32>()).prop_map(
        move |(seqnum, devid, direction, ep)| UsbipHeaderBasic {
            command: command as u32,
            seqnum,
            devid,
            direction,
            ep,
        },
    )
}

fn arb_transfer_buffer(max_len: usize) -> impl Strategy<Value = (u32, Bytes)> {
    prop::collection::vec(any::<u8>(), 0..max_len).prop_map(|v| {
        let len = v.len() as u32;
        (len, Bytes::from(v))
    })
}

// ── Round-trip property tests ───────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // -- Discovery messages --

    #[test]
    fn roundtrip_op_req_devlist(_dummy in 0u8..1u8) {
        let msg = OpReqDevlist;
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        prop_assert_eq!(buf.len(), 8);
        let mut cursor = &buf[..];
        let decoded = OpReqDevlist::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_op_rep_devlist(devices in prop::collection::vec(arb_device(), 0..5)) {
        let msg = OpRepDevlist {
            status: 0,
            devices,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = OpRepDevlist::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_op_req_import(busid in arb_busid()) {
        let msg = OpReqImport { busid };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        prop_assert_eq!(buf.len(), 40);
        let mut cursor = &buf[..];
        let decoded = OpReqImport::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_op_rep_import_success(device in arb_device()) {
        // Import reply doesn't include interfaces in the wire format,
        // so strip them and set num_interfaces=0 for the decode comparison.
        let mut device = device;
        device.interfaces.clear();
        device.num_interfaces = 0;

        let msg = OpRepImport {
            status: 0,
            device: Some(device),
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = OpRepImport::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_op_rep_import_error(status in 1u32..100u32) {
        let msg = OpRepImport {
            status,
            device: None,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = OpRepImport::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    // -- URB messages --

    #[test]
    fn roundtrip_cmd_submit_out(
        header in arb_header_basic(Command::CmdSubmit).prop_map(|mut h| { h.direction = 0; h }),
        transfer_flags in any::<u32>(),
        (transfer_buffer_length, transfer_buffer) in arb_transfer_buffer(1024),
        start_frame in any::<u32>(),
        interval in any::<u32>(),
        setup in prop::array::uniform8(any::<u8>()),
    ) {
        let msg = CmdSubmit {
            header,
            transfer_flags,
            transfer_buffer_length,
            start_frame,
            number_of_packets: 0xFFFF_FFFF, // non-ISO
            interval,
            setup,
            transfer_buffer,
            iso_packet_descriptors: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = CmdSubmit::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_cmd_submit_in(
        header in arb_header_basic(Command::CmdSubmit).prop_map(|mut h| { h.direction = 1; h }),
        transfer_flags in any::<u32>(),
        transfer_buffer_length in 0u32..=1_048_576u32,
        start_frame in any::<u32>(),
        interval in any::<u32>(),
        setup in prop::array::uniform8(any::<u8>()),
    ) {
        let msg = CmdSubmit {
            header,
            transfer_flags,
            transfer_buffer_length,
            start_frame,
            number_of_packets: 0xFFFF_FFFF, // non-ISO
            interval,
            setup,
            transfer_buffer: Bytes::new(), // IN requests have no payload
            iso_packet_descriptors: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = CmdSubmit::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_ret_submit_in(
        header in arb_header_basic(Command::RetSubmit).prop_map(|mut h| { h.direction = 1; h }),
        status in any::<i32>(),
        (actual_length, transfer_buffer) in arb_transfer_buffer(1024),
        start_frame in any::<u32>(),
        error_count in any::<u32>(),
    ) {
        let msg = RetSubmit {
            header,
            status,
            actual_length,
            start_frame,
            number_of_packets: 0xFFFF_FFFF, // non-ISO
            error_count,
            transfer_buffer,
            iso_packet_descriptors: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = RetSubmit::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_ret_submit_out(
        header in arb_header_basic(Command::RetSubmit).prop_map(|mut h| { h.direction = 0; h }),
        status in any::<i32>(),
        actual_length in 0u32..=1_048_576u32,
        start_frame in any::<u32>(),
        error_count in any::<u32>(),
    ) {
        let msg = RetSubmit {
            header,
            status,
            actual_length,
            start_frame,
            number_of_packets: 0xFFFF_FFFF, // non-ISO
            error_count,
            transfer_buffer: Bytes::new(), // OUT returns have no payload
            iso_packet_descriptors: vec![],
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        let mut cursor = &buf[..];
        let decoded = RetSubmit::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_cmd_unlink(
        header in arb_header_basic(Command::CmdUnlink),
        unlink_seqnum in any::<u32>(),
    ) {
        let msg = CmdUnlink {
            header,
            unlink_seqnum,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        prop_assert_eq!(buf.len(), 48);
        let mut cursor = &buf[..];
        let decoded = CmdUnlink::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_ret_unlink(
        header in arb_header_basic(Command::RetUnlink),
        status in any::<i32>(),
    ) {
        let msg = RetUnlink {
            header,
            status,
        };
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        prop_assert_eq!(buf.len(), 48);
        let mut cursor = &buf[..];
        let decoded = RetUnlink::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, msg);
    }

    // -- UsbDevice and UsbInterface --

    #[test]
    fn roundtrip_usb_interface(iface in arb_interface()) {
        let mut buf = Vec::new();
        iface.encode(&mut buf);
        prop_assert_eq!(buf.len(), 4);
        let mut cursor = &buf[..];
        let decoded = UsbInterface::decode(&mut cursor).unwrap();
        prop_assert_eq!(decoded, iface);
    }

    #[test]
    fn roundtrip_usb_device_with_interfaces(device in arb_device()) {
        let mut buf = Vec::new();
        device::encode_device_with_interfaces(&device, &mut buf);
        let mut cursor = &buf[..];
        let decoded = device::decode_device_with_interfaces(&mut cursor).unwrap();
        prop_assert_eq!(decoded, device);
    }
}

// ── Golden byte tests ───────────────────────────────────────────────

/// Test OP_REQ_DEVLIST against known byte sequence.
#[test]
fn golden_op_req_devlist() {
    let expected: [u8; 8] = [
        0x01, 0x11, // version 0x0111
        0x80, 0x05, // OP_REQ_DEVLIST
        0x00, 0x00, 0x00, 0x00, // status = 0
    ];
    let msg = OpReqDevlist;
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf, expected);

    let mut cursor = &expected[..];
    let decoded = OpReqDevlist::decode(&mut cursor).unwrap();
    assert_eq!(decoded, OpReqDevlist);
}

/// Test OP_REQ_IMPORT against known byte sequence.
#[test]
fn golden_op_req_import() {
    let mut expected = vec![
        0x01, 0x11, // version 0x0111
        0x80, 0x03, // OP_REQ_IMPORT
        0x00, 0x00, 0x00, 0x00, // status = 0
    ];
    // busid = "1-1" followed by 29 null bytes
    let mut busid_bytes = vec![0u8; 32];
    busid_bytes[0] = b'1';
    busid_bytes[1] = b'-';
    busid_bytes[2] = b'1';
    expected.extend_from_slice(&busid_bytes);

    let busid = UsbDevice::busid_from_str("1-1").unwrap();
    let msg = OpReqImport { busid };
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf, expected);

    let mut cursor = &expected[..];
    let decoded = OpReqImport::decode(&mut cursor).unwrap();
    assert_eq!(decoded.busid, busid);
}

/// Test OP_REP_DEVLIST with zero devices against known byte sequence.
#[test]
fn golden_op_rep_devlist_empty() {
    let expected: [u8; 12] = [
        0x01, 0x11, // version
        0x00, 0x05, // OP_REP_DEVLIST
        0x00, 0x00, 0x00, 0x00, // status = 0
        0x00, 0x00, 0x00, 0x00, // num_devices = 0
    ];
    let msg = OpRepDevlist {
        status: 0,
        devices: vec![],
    };
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf, expected);
}

/// Test OP_REP_IMPORT error against known byte sequence.
#[test]
fn golden_op_rep_import_error() {
    let expected: [u8; 8] = [
        0x01, 0x11, // version
        0x00, 0x03, // OP_REP_IMPORT
        0x00, 0x00, 0x00, 0x01, // status = 1 (error)
    ];
    let msg = OpRepImport {
        status: 1,
        device: None,
    };
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf, expected);
}

/// Test CMD_SUBMIT header against known byte sequence.
#[test]
fn golden_cmd_submit_control_in() {
    // GET_DESCRIPTOR control transfer: IN, ep=0, 64-byte buffer
    let msg = CmdSubmit {
        header: UsbipHeaderBasic {
            command: Command::CmdSubmit as u32,
            seqnum: 1,
            devid: 0x00010002,
            direction: 1, // IN
            ep: 0,
        },
        transfer_flags: 0x00000200, // URB_SHORT_NOT_OK
        transfer_buffer_length: 64,
        start_frame: 0,
        number_of_packets: 0xFFFFFFFF,
        interval: 0,
        setup: [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00],
        transfer_buffer: Bytes::new(),
        iso_packet_descriptors: vec![],
    };

    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf.len(), 48); // No payload for IN

    // Verify specific byte positions
    // command = 0x00000001 at offset 0
    assert_eq!(&buf[0..4], &[0x00, 0x00, 0x00, 0x01]);
    // seqnum = 1 at offset 4
    assert_eq!(&buf[4..8], &[0x00, 0x00, 0x00, 0x01]);
    // devid at offset 8
    assert_eq!(&buf[8..12], &[0x00, 0x01, 0x00, 0x02]);
    // direction = IN = 1 at offset 12
    assert_eq!(&buf[12..16], &[0x00, 0x00, 0x00, 0x01]);
    // ep = 0 at offset 16
    assert_eq!(&buf[16..20], &[0x00, 0x00, 0x00, 0x00]);
    // transfer_flags at offset 20
    assert_eq!(&buf[20..24], &[0x00, 0x00, 0x02, 0x00]);
    // buffer_length = 64 at offset 24
    assert_eq!(&buf[24..28], &[0x00, 0x00, 0x00, 0x40]);
    // setup at offset 40
    assert_eq!(
        &buf[40..48],
        &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00]
    );
}

/// Test RET_UNLINK with -ECONNRESET against known byte sequence.
#[test]
fn golden_ret_unlink_econnreset() {
    let msg = RetUnlink {
        header: UsbipHeaderBasic {
            command: Command::RetUnlink as u32,
            seqnum: 5,
            devid: 0x00010002,
            direction: 0,
            ep: 0,
        },
        status: -104, // -ECONNRESET
    };

    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf.len(), 48);

    // command = 0x00000004 (RET_UNLINK)
    assert_eq!(&buf[0..4], &[0x00, 0x00, 0x00, 0x04]);
    // seqnum = 5
    assert_eq!(&buf[4..8], &[0x00, 0x00, 0x00, 0x05]);
    // status = -104 = 0xFFFFFF98
    assert_eq!(&buf[20..24], &[0xFF, 0xFF, 0xFF, 0x98]);
    // padding is all zeros
    assert!(buf[24..48].iter().all(|&b| b == 0));
}

/// Test CMD_UNLINK against known byte sequence.
#[test]
fn golden_cmd_unlink() {
    let msg = CmdUnlink {
        header: UsbipHeaderBasic {
            command: Command::CmdUnlink as u32,
            seqnum: 5,
            devid: 0x00010002,
            direction: 0,
            ep: 0,
        },
        unlink_seqnum: 3,
    };

    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf.len(), 48);

    // command = 0x00000002 (CMD_UNLINK)
    assert_eq!(&buf[0..4], &[0x00, 0x00, 0x00, 0x02]);
    // unlink_seqnum = 3 at offset 20
    assert_eq!(&buf[20..24], &[0x00, 0x00, 0x00, 0x03]);
    // padding is all zeros
    assert!(buf[24..48].iter().all(|&b| b == 0));
}

/// Test RET_SUBMIT with IN data against known byte sequence.
#[test]
fn golden_ret_submit_in_with_data() {
    let data = vec![0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40];
    let msg = RetSubmit {
        header: UsbipHeaderBasic {
            command: Command::RetSubmit as u32,
            seqnum: 1,
            devid: 0x00010002,
            direction: 1, // IN
            ep: 0,
        },
        status: 0,
        actual_length: 8,
        start_frame: 0,
        number_of_packets: 0xFFFFFFFF,
        error_count: 0,
        transfer_buffer: Bytes::from(data.clone()),
        iso_packet_descriptors: vec![],
    };

    let mut buf = Vec::new();
    msg.encode(&mut buf);
    assert_eq!(buf.len(), 48 + 8);

    // command = 0x00000003 (RET_SUBMIT)
    assert_eq!(&buf[0..4], &[0x00, 0x00, 0x00, 0x03]);
    // status = 0
    assert_eq!(&buf[20..24], &[0x00, 0x00, 0x00, 0x00]);
    // actual_length = 8
    assert_eq!(&buf[24..28], &[0x00, 0x00, 0x00, 0x08]);
    // transfer data at offset 48
    assert_eq!(&buf[48..56], &data[..]);
}

// ── Rejection tests ─────────────────────────────────────────────────

#[test]
fn reject_empty_buffer_header_basic() {
    let buf: &[u8] = &[];
    let mut cursor = buf;
    let result = UsbipHeaderBasic::decode(&mut cursor);
    assert!(matches!(
        result,
        Err(ProtocolError::BufferTooShort {
            needed: 20,
            available: 0
        })
    ));
}

#[test]
fn reject_truncated_cmd_submit() {
    // Only the header, missing the submit-specific fields
    let header = UsbipHeaderBasic {
        command: Command::CmdSubmit as u32,
        seqnum: 1,
        devid: 2,
        direction: 0,
        ep: 0,
    };
    let mut buf = Vec::new();
    header.encode(&mut buf);
    // Add only 10 bytes instead of 28
    buf.extend_from_slice(&[0u8; 10]);

    let mut cursor = &buf[..];
    // Decode header first
    let _ = UsbipHeaderBasic::decode(&mut cursor).unwrap();
    // Not enough for submit fields
    // We'll test via the full CmdSubmit::decode path
    let mut cursor = &buf[..];
    let result = CmdSubmit::decode(&mut cursor);
    assert!(matches!(result, Err(ProtocolError::BufferTooShort { .. })));
}

#[test]
fn reject_wrong_version() {
    let buf: [u8; 8] = [
        0x02, 0x00, // wrong version
        0x80, 0x05, // correct opcode
        0x00, 0x00, 0x00, 0x00,
    ];
    let mut cursor = &buf[..];
    let result = OpReqDevlist::decode(&mut cursor);
    assert!(matches!(
        result,
        Err(ProtocolError::UnsupportedVersion(0x0200))
    ));
}

#[test]
fn reject_wrong_opcode_in_decode() {
    let buf: [u8; 8] = [
        0x01, 0x11, // correct version
        0x00, 0x03, // OP_REP_IMPORT instead of OP_REQ_DEVLIST
        0x00, 0x00, 0x00, 0x00,
    ];
    let mut cursor = &buf[..];
    let result = OpReqDevlist::decode(&mut cursor);
    assert!(matches!(result, Err(ProtocolError::InvalidOpCode(0x0003))));
}

#[test]
fn reject_invalid_command_code() {
    // Put command=0x00000099 which is not valid
    let mut buf = Vec::new();
    buf.extend_from_slice(&0x00000099u32.to_be_bytes()); // invalid command
    buf.extend_from_slice(&[0u8; 44]); // rest of 48 bytes

    let mut cursor = &buf[..];
    let result = CmdSubmit::decode(&mut cursor);
    assert!(matches!(result, Err(ProtocolError::InvalidCommand(0x99))));
}

#[test]
fn reject_truncated_device_descriptor() {
    let buf = [0u8; 100]; // only 100 bytes, need 312
    let mut cursor = &buf[..];
    let result = UsbDevice::decode(&mut cursor);
    assert!(matches!(
        result,
        Err(ProtocolError::BufferTooShort {
            needed: 312,
            available: 100
        })
    ));
}

#[test]
fn reject_truncated_interface() {
    let buf = [0u8; 2]; // only 2 bytes, need 4
    let mut cursor = &buf[..];
    let result = UsbInterface::decode(&mut cursor);
    assert!(matches!(
        result,
        Err(ProtocolError::BufferTooShort {
            needed: 4,
            available: 2
        })
    ));
}

#[test]
fn reject_cmd_submit_with_missing_transfer_buffer() {
    // OUT transfer with buffer_length=100 but no actual data
    let msg_header = UsbipHeaderBasic {
        command: Command::CmdSubmit as u32,
        seqnum: 1,
        devid: 2,
        direction: 0, // OUT -- buffer expected
        ep: 1,
    };
    let mut buf = Vec::new();
    msg_header.encode(&mut buf);
    buf.extend_from_slice(&0u32.to_be_bytes()); // transfer_flags
    buf.extend_from_slice(&100u32.to_be_bytes()); // transfer_buffer_length = 100
    buf.extend_from_slice(&0u32.to_be_bytes()); // start_frame
    buf.extend_from_slice(&0u32.to_be_bytes()); // number_of_packets
    buf.extend_from_slice(&0u32.to_be_bytes()); // interval
    buf.extend_from_slice(&[0u8; 8]); // setup
                                      // No transfer buffer data!

    let mut cursor = &buf[..];
    let result = CmdSubmit::decode(&mut cursor);
    assert!(matches!(result, Err(ProtocolError::BufferTooShort { .. })));
}
