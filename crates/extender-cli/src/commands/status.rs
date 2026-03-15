//! `extender status` subcommand — show daemon status and device overview.

use extender_api::types::{DaemonStatus, ExportedDeviceInfo, ImportedDeviceInfo};
use extender_api::ApiMethod;

use crate::client;
use crate::output::{self, OutputFormat};

/// Execute the status command.
pub async fn run(
    socket_path: &str,
    format: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    // Fetch status, exported devices, and imported devices (sequentially over
    // the same socket path — each call opens a new connection).
    let status_val = client::call_daemon(socket_path, ApiMethod::GetStatus).await?;
    let status: DaemonStatus = serde_json::from_value(status_val)?;

    let exported_val = client::call_daemon(socket_path, ApiMethod::ListExportedDevices).await?;
    let exported: Vec<ExportedDeviceInfo> = serde_json::from_value(exported_val)?;

    // For imported devices we reuse ListLocalDevices conceptually, but the API
    // doesn't have a dedicated "list imported" method. We'll show what we have
    // from the status counts and the exported list. If a ListImportedDevices
    // method is added later, we can call it here. For now, use an empty vec
    // and rely on the status counts.
    let imported: Vec<ImportedDeviceInfo> = Vec::new();

    output::print_status(&status, &exported, &imported, format);
    Ok(())
}
