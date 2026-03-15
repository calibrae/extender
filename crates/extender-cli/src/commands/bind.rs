//! `extender bind` and `extender unbind` subcommands.

use extender_api::ApiMethod;

use crate::client;
use crate::output::{self, OutputFormat};

/// Execute the bind command — export a device for remote access.
pub async fn run_bind(
    socket_path: &str,
    format: OutputFormat,
    bus_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    client::call_daemon(
        socket_path,
        ApiMethod::BindDevice {
            bus_id: bus_id.to_string(),
        },
    )
    .await?;
    output::print_ok(&format!("Device {bus_id} exported successfully."), format);
    Ok(())
}

/// Execute the unbind command — stop exporting a device.
pub async fn run_unbind(
    socket_path: &str,
    format: OutputFormat,
    bus_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    client::call_daemon(
        socket_path,
        ApiMethod::UnbindDevice {
            bus_id: bus_id.to_string(),
        },
    )
    .await?;
    output::print_ok(&format!("Device {bus_id} unexported successfully."), format);
    Ok(())
}
