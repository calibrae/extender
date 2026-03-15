//! `extender list` subcommand — list local or remote USB devices.

use extender_api::types::DeviceInfo;
use extender_api::ApiMethod;

use crate::client;
use crate::output::{self, OutputFormat};

/// Default USB/IP port.
const DEFAULT_PORT: u16 = 3240;

/// Execute the list command.
pub async fn run(
    socket_path: &str,
    format: OutputFormat,
    local: bool,
    remote: Option<&str>,
    port: Option<u16>,
) -> Result<(), Box<dyn std::error::Error>> {
    if local || remote.is_none() {
        // List local devices (default when neither flag is given, or --local).
        let result = client::call_daemon(socket_path, ApiMethod::ListLocalDevices).await?;
        let devices: Vec<DeviceInfo> = serde_json::from_value(result)?;
        output::print_devices(&devices, format);
    } else if let Some(host) = remote {
        // List remote devices.
        let port = port.unwrap_or(DEFAULT_PORT);
        let result = client::call_daemon(
            socket_path,
            ApiMethod::ListRemoteDevices {
                host: host.to_string(),
                port,
            },
        )
        .await?;
        let devices: Vec<DeviceInfo> = serde_json::from_value(result)?;
        output::print_devices(&devices, format);
    }
    Ok(())
}
