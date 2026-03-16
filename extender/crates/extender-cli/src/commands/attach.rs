//! `extender attach` and `extender detach` subcommands.

use extender_api::ApiMethod;

use crate::client;
use crate::output::{self, OutputFormat};

/// Default USB/IP port.
const DEFAULT_PORT: u16 = 3240;

/// Execute the attach command — import a remote USB device.
pub async fn run_attach(
    socket_path: &str,
    format: OutputFormat,
    host: &str,
    bus_id: &str,
    port: Option<u16>,
) -> Result<(), Box<dyn std::error::Error>> {
    let port = port.unwrap_or(DEFAULT_PORT);
    client::call_daemon(
        socket_path,
        ApiMethod::AttachDevice {
            host: host.to_string(),
            port,
            bus_id: bus_id.to_string(),
        },
    )
    .await?;
    output::print_ok(&format!("Attached {bus_id} from {host}:{port}."), format);
    Ok(())
}

/// Execute the detach command — disconnect an imported device.
pub async fn run_detach(
    socket_path: &str,
    format: OutputFormat,
    vhci_port: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    client::call_daemon(socket_path, ApiMethod::DetachDevice { port: vhci_port }).await?;
    output::print_ok(&format!("Detached device on port {vhci_port}."), format);
    Ok(())
}
