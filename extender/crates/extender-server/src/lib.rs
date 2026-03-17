//! USB/IP server logic: device enumeration, handle management, transfer execution,
//! TCP listener, export registry, and URB forwarding.
//!
//! This crate provides both the USB hardware abstraction layer and the network
//! server for the Extender server component. It wraps `rusb` (libusb bindings)
//! for local USB access and uses `tokio` for async TCP handling.
//!
//! # Modules
//!
//! - [`device`] -- Local USB device enumeration and filtering
//! - [`handle`] -- Managed device handles with Drop-based cleanup
//! - [`transfer`] -- USB transfer execution (control, bulk, interrupt)
//! - [`error`] -- Server error types and rusb-to-errno mapping
//! - [`export`] -- Device export registry (bind/unbind)
//! - [`engine`] -- TCP listener and connection accept loop
//! - [`connection`] -- Per-connection handler (DEVLIST, IMPORT dispatch)
//! - [`session`] -- Per-device URB forwarding loop

pub mod connection;
pub mod device;
pub mod engine;
pub mod error;
pub mod export;
pub mod handle;
pub mod session;
pub mod tls;
pub mod transfer;

// Re-export key types for convenience.
pub use device::{enumerate_devices, filter_devices, DeviceFilter, LocalUsbDevice};
pub use engine::ServerEngine;
pub use error::ServerError;
pub use export::{ExportRegistry, ExportedDevice, SessionId};
pub use handle::ManagedDevice;
pub use session::DeviceSession;
pub use tls::TlsServerConfig;
pub use transfer::{
    execute_bulk_transfer, execute_control_transfer, execute_interrupt_transfer, TransferResult,
};
