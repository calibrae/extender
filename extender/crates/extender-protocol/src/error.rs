//! Protocol error types.

use thiserror::Error;

/// Errors that can occur during protocol encoding/decoding.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// An invalid or unrecognized command/op code was encountered.
    #[error("invalid command code: 0x{0:08x}")]
    InvalidCommand(u32),

    /// An invalid or unrecognized op code was encountered.
    #[error("invalid op code: 0x{0:04x}")]
    InvalidOpCode(u16),

    /// The buffer does not contain enough data for the expected message.
    #[error("buffer too short: need {needed} bytes, have {available}")]
    BufferTooShort { needed: usize, available: usize },

    /// A bus ID field contains invalid data (not null-terminated or not ASCII).
    #[error("invalid bus ID: {0}")]
    InvalidBusId(String),

    /// The protocol version in the message header does not match 0x0111.
    #[error("unsupported protocol version: 0x{0:04x}")]
    UnsupportedVersion(u16),

    /// An I/O error occurred during async read/write.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The status field indicates an error from the remote side.
    #[error("remote status error: {0}")]
    RemoteError(u32),

    /// Transfer buffer length exceeds the maximum allowed size.
    #[error("transfer buffer too large: {length} bytes (max {max})")]
    TransferTooLarge { length: u32, max: u32 },

    /// Device count exceeds the maximum allowed in a DEVLIST reply.
    #[error("too many devices in DEVLIST: {count} (max {max})")]
    TooManyDevices { count: u32, max: u32 },

    /// Too many ISO packet descriptors.
    #[error("too many ISO packets: {count} (max {max})")]
    TooManyIsoPackets { count: u32, max: u32 },
}
