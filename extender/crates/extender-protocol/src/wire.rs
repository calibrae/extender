//! WireFormat trait for encoding/decoding protocol messages.

use bytes::{Buf, BufMut};

use crate::ProtocolError;

/// Trait for types that can be serialized to/from the USB/IP wire format.
///
/// All multi-byte fields are big-endian (network byte order).
pub trait WireFormat: Sized {
    /// Encode this message into the buffer.
    fn encode(&self, buf: &mut impl BufMut);

    /// Decode a message from the buffer.
    fn decode(buf: &mut impl Buf) -> Result<Self, ProtocolError>;

    /// The size in bytes of this message on the wire.
    /// For variable-length messages, this returns the size of the current instance.
    fn wire_size(&self) -> usize;
}
