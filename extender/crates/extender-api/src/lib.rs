//! Shared API types for the Extender JSON-RPC interface.
//!
//! This crate defines the request/response types used for communication
//! between the CLI and daemon over a Unix domain socket.

pub mod jsonrpc;
pub mod methods;
pub mod types;

pub use jsonrpc::{
    read_message, write_message, FramingError, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    MAX_MESSAGE_SIZE,
};
pub use methods::{ApiMethod, ApiResponse};
pub use types::{
    DaemonEvent, DaemonStatus, DeviceInfo, ExportedDeviceInfo, ImportedDeviceInfo, UsbSpeed,
};
