//! USB/IP server logic: device enumeration, handle management, and transfer execution.
//!
//! This crate provides the USB hardware abstraction layer for the Extender
//! server. It wraps `rusb` (libusb bindings) to enumerate local USB devices,
//! manage device handles with automatic interface claiming/release, and execute
//! USB transfers with proper error mapping to USB/IP status codes.
//!
//! # Modules
//!
//! - [`device`] -- Local USB device enumeration and filtering
//! - [`handle`] -- Managed device handles with Drop-based cleanup
//! - [`transfer`] -- USB transfer execution (control, bulk, interrupt)
//! - [`error`] -- Server error types and rusb-to-errno mapping

pub mod device;
pub mod error;
pub mod handle;
pub mod transfer;

// Re-export key types for convenience.
pub use device::{enumerate_devices, filter_devices, DeviceFilter, LocalUsbDevice};
pub use error::ServerError;
pub use handle::ManagedDevice;
pub use transfer::{
    execute_bulk_transfer, execute_control_transfer, execute_interrupt_transfer, TransferResult,
};
