//! USB/IP v1.1.1 wire format types and serialization.
//!
//! This crate implements the binary protocol defined by the Linux kernel's
//! USB/IP subsystem. All types serialize to/from big-endian (network byte order)
//! using the `bytes` crate's `Buf`/`BufMut` traits.
//!
//! No serde is used for wire format -- serialization is manual to match
//! the exact byte layout specified by the kernel.

pub mod codec;
pub mod codes;
pub mod device;
pub mod discovery;
pub mod error;
pub mod urb;
pub mod wire;

pub use codes::*;
pub use device::*;
pub use discovery::*;
pub use error::*;
pub use urb::*;
pub use wire::WireFormat;
