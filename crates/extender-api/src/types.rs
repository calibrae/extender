//! Domain types shared between the CLI and daemon.

use serde::{Deserialize, Serialize};

/// USB speed classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsbSpeed {
    Low,
    Full,
    High,
    Super,
    SuperPlus,
    Unknown,
}

/// Information about a locally-connected USB device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub bus_id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub device_class: u8,
    pub speed: UsbSpeed,
    pub is_bound: bool,
}

/// Information about a device that has been exported (bound) by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedDeviceInfo {
    pub bus_id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub device_class: u8,
    pub speed: UsbSpeed,
    pub num_clients: u32,
}

/// Information about a device that has been imported (attached) by the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportedDeviceInfo {
    pub port: u32,
    pub host: String,
    pub remote_bus_id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub speed: UsbSpeed,
}

/// Overall daemon status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub uptime_secs: u64,
    pub exported_devices: u32,
    pub imported_devices: u32,
    pub active_connections: u32,
}

/// A notification event sent to subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DaemonEvent {
    DevicePlugged(DeviceInfo),
    DeviceUnplugged { bus_id: String },
    DeviceBound { bus_id: String },
    DeviceUnbound { bus_id: String },
    ClientConnected { remote_addr: String },
    ClientDisconnected { remote_addr: String },
    Error { message: String },
}
