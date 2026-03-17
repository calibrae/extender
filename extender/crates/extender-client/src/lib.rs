//! USB/IP client logic: device import and vhci_hcd interaction.
//!
//! This crate provides the client-side logic for USB/IP. It can:
//!
//! - Query a remote server for its list of exported USB devices
//! - Import (attach) a remote device through the vhci_hcd kernel module (Linux)
//! - Detach a previously imported device
//! - List currently imported devices
//!
//! On non-Linux platforms, attach/detach operations return
//! `ClientError::PlatformNotSupported`.

pub mod discover;
pub mod engine;
pub mod error;
pub mod mass_storage;
pub mod reconnect;
pub mod remote;
pub mod tls;
pub mod types;

#[cfg(target_os = "linux")]
pub mod vhci;

// Re-export key types for convenience.
pub use discover::{discover_servers, DiscoveredServer};
pub use engine::ClientEngine;
pub use error::ClientError;
pub use reconnect::{attach_with_reconnect, ReconnectPolicy};
pub use remote::list_remote_devices;
pub use tls::TlsClientConfig;
pub use types::{AttachedDevice, ImportedDevice, PortStatus, RemoteDevice, VhciPort};
