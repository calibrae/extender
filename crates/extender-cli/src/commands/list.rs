//! `extender list` subcommand — list local or remote USB devices.

use std::net::SocketAddr;

use extender_api::types::{DeviceInfo, UsbSpeed};
use extender_api::ApiMethod;

use crate::client;
use crate::output::{self, OutputFormat};

/// Default USB/IP port.
const DEFAULT_PORT: u16 = 3240;

/// Execute the list command.
pub async fn run(
    socket_path: &str,
    format: OutputFormat,
    _local: bool,
    remote: Option<&str>,
    port: Option<u16>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(host) = remote {
        // List remote devices — connect directly to remote USB/IP server via TCP.
        // This does NOT require a local daemon.
        let port = port.unwrap_or(DEFAULT_PORT);
        let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
        let remote_devices = extender_client::remote::list_remote_devices(addr).await?;
        let devices: Vec<DeviceInfo> = remote_devices
            .iter()
            .map(|d| DeviceInfo {
                bus_id: d.busid.clone(),
                vendor_id: d.id_vendor,
                product_id: d.id_product,
                manufacturer: None, // wire protocol doesn't carry string descriptors
                product: None,
                device_class: d.device_class,
                speed: match d.speed {
                    1 => UsbSpeed::Low,
                    2 => UsbSpeed::Full,
                    3 => UsbSpeed::High,
                    5 => UsbSpeed::Super,
                    _ => UsbSpeed::Unknown,
                },
                is_bound: false,
            })
            .collect();
        output::print_devices(&devices, format);
    } else {
        // List local devices (default, or --local).
        let result = client::call_daemon(socket_path, ApiMethod::ListLocalDevices).await?;
        let devices: Vec<DeviceInfo> = serde_json::from_value(result)?;
        output::print_devices(&devices, format);
    }
    Ok(())
}
