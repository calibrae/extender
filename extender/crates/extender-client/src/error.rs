//! Client error types.

use std::net::SocketAddr;

use thiserror::Error;

/// Errors that can occur in the USB/IP client.
#[derive(Debug, Error)]
pub enum ClientError {
    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A protocol-level error occurred.
    #[error("protocol error: {0}")]
    Protocol(#[from] extender_protocol::ProtocolError),

    /// Connection to the remote server timed out.
    #[error("connection to {addr} timed out after {timeout_secs}s")]
    ConnectTimeout { addr: SocketAddr, timeout_secs: u64 },

    /// The remote server returned an error status for a DEVLIST request.
    #[error("remote server returned error status {status} for device list")]
    DevlistError { status: u32 },

    /// The remote server returned an error status for an IMPORT request.
    #[error("remote server rejected import of bus ID '{busid}' with status {status}")]
    ImportRejected { busid: String, status: u32 },

    /// The import reply did not include a device descriptor.
    #[error("import reply missing device descriptor")]
    ImportMissingDevice,

    /// The bus ID string is invalid.
    #[error("invalid bus ID: {0}")]
    InvalidBusId(String),

    /// No free VHCI port is available for the requested speed.
    #[error("no free VHCI port available for speed {speed}")]
    NoFreePort { speed: u32 },

    /// The specified port is not currently in use.
    #[error("port {port} is not attached")]
    PortNotAttached { port: u32 },

    /// VHCI sysfs interface is not available (e.g., module not loaded).
    #[error("VHCI driver not available: {reason}")]
    VhciNotAvailable { reason: String },

    /// Client import is not supported on this platform.
    #[error("client import not supported on this platform yet")]
    PlatformNotSupported,

    /// A TLS configuration or connection error.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Error parsing VHCI status file.
    #[error("failed to parse VHCI status: {reason}")]
    VhciParseError { reason: String },

    /// A USB Mass Storage protocol error.
    #[error("mass storage error: {0}")]
    MassStorage(String),
}
