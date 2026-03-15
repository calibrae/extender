//! API method and response enumerations.

use serde::{Deserialize, Serialize};

use crate::types::{DaemonStatus, DeviceInfo, ExportedDeviceInfo, ImportedDeviceInfo};

/// All API methods the daemon can handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ApiMethod {
    ListLocalDevices,
    ListExportedDevices,
    ListRemoteDevices {
        host: String,
        port: u16,
    },
    BindDevice {
        bus_id: String,
    },
    UnbindDevice {
        bus_id: String,
    },
    AttachDevice {
        host: String,
        port: u16,
        bus_id: String,
    },
    DetachDevice {
        port: u32,
    },
    GetStatus,
    GetDeviceInfo {
        bus_id: String,
    },
    Subscribe {
        events: Vec<String>,
    },
}

impl ApiMethod {
    /// Returns the JSON-RPC method name string for this variant.
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::ListLocalDevices => "list_local_devices",
            Self::ListExportedDevices => "list_exported_devices",
            Self::ListRemoteDevices { .. } => "list_remote_devices",
            Self::BindDevice { .. } => "bind_device",
            Self::UnbindDevice { .. } => "unbind_device",
            Self::AttachDevice { .. } => "attach_device",
            Self::DetachDevice { .. } => "detach_device",
            Self::GetStatus => "get_status",
            Self::GetDeviceInfo { .. } => "get_device_info",
            Self::Subscribe { .. } => "subscribe",
        }
    }
}

/// Successful API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ApiResponse {
    Devices(Vec<DeviceInfo>),
    ExportedDevices(Vec<ExportedDeviceInfo>),
    ImportedDevices(Vec<ImportedDeviceInfo>),
    DeviceInfo(DeviceInfo),
    Status(DaemonStatus),
    Ok,
}
