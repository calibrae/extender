//! USB/IP protocol version, op codes, and command codes.

/// USB/IP protocol version v1.1.1, encoded as 0x0111.
pub const USBIP_VERSION: u16 = 0x0111;

/// Default TCP port for USB/IP.
pub const USBIP_PORT: u16 = 3240;

/// Operation codes for the discovery phase of the protocol.
///
/// These are used in the 2-byte command field of discovery message headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum OpCode {
    /// Request the list of exported devices.
    OpReqDevlist = 0x8005,
    /// Reply with the list of exported devices.
    OpRepDevlist = 0x0005,
    /// Request to import (attach) a specific device.
    OpReqImport = 0x8003,
    /// Reply to an import request.
    OpRepImport = 0x0003,
}

impl OpCode {
    /// Try to convert a raw u16 value into an OpCode.
    pub fn from_raw(value: u16) -> Option<Self> {
        match value {
            0x8005 => Some(OpCode::OpReqDevlist),
            0x0005 => Some(OpCode::OpRepDevlist),
            0x8003 => Some(OpCode::OpReqImport),
            0x0003 => Some(OpCode::OpRepImport),
            _ => None,
        }
    }
}

/// Command codes for the URB transfer phase of the protocol.
///
/// These are used in the 4-byte command field of `UsbipHeaderBasic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Command {
    /// Submit a USB request block (URB) to the server.
    CmdSubmit = 0x0000_0001,
    /// Cancel a previously submitted URB.
    CmdUnlink = 0x0000_0002,
    /// Return the result of a submitted URB.
    RetSubmit = 0x0000_0003,
    /// Return the result of an unlink request.
    RetUnlink = 0x0000_0004,
}

impl Command {
    /// Try to convert a raw u32 value into a Command.
    pub fn from_raw(value: u32) -> Option<Self> {
        match value {
            0x0000_0001 => Some(Command::CmdSubmit),
            0x0000_0002 => Some(Command::CmdUnlink),
            0x0000_0003 => Some(Command::RetSubmit),
            0x0000_0004 => Some(Command::RetUnlink),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_op_code_values() {
        assert_eq!(OpCode::OpReqDevlist as u16, 0x8005);
        assert_eq!(OpCode::OpRepDevlist as u16, 0x0005);
        assert_eq!(OpCode::OpReqImport as u16, 0x8003);
        assert_eq!(OpCode::OpRepImport as u16, 0x0003);
    }

    #[test]
    fn test_command_values() {
        assert_eq!(Command::CmdSubmit as u32, 0x0000_0001);
        assert_eq!(Command::CmdUnlink as u32, 0x0000_0002);
        assert_eq!(Command::RetSubmit as u32, 0x0000_0003);
        assert_eq!(Command::RetUnlink as u32, 0x0000_0004);
    }

    #[test]
    fn test_op_code_from_raw() {
        assert_eq!(OpCode::from_raw(0x8005), Some(OpCode::OpReqDevlist));
        assert_eq!(OpCode::from_raw(0x0005), Some(OpCode::OpRepDevlist));
        assert_eq!(OpCode::from_raw(0x8003), Some(OpCode::OpReqImport));
        assert_eq!(OpCode::from_raw(0x0003), Some(OpCode::OpRepImport));
        assert_eq!(OpCode::from_raw(0xFFFF), None);
    }

    #[test]
    fn test_command_from_raw() {
        assert_eq!(Command::from_raw(1), Some(Command::CmdSubmit));
        assert_eq!(Command::from_raw(2), Some(Command::CmdUnlink));
        assert_eq!(Command::from_raw(3), Some(Command::RetSubmit));
        assert_eq!(Command::from_raw(4), Some(Command::RetUnlink));
        assert_eq!(Command::from_raw(0), None);
        assert_eq!(Command::from_raw(5), None);
    }
}
