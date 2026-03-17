//! Output formatters for human-readable tables, JSON, and quiet modes.

use extender_api::types::{
    DaemonStatus, DeviceInfo, ExportedDeviceInfo, ImportedDeviceInfo, UsbSpeed,
};

/// Output format selected by the user.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Quiet,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn speed_str(speed: &UsbSpeed) -> &'static str {
    match speed {
        UsbSpeed::Low => "1.5 Mbps",
        UsbSpeed::Full => "12 Mbps",
        UsbSpeed::High => "480 Mbps",
        UsbSpeed::Super => "5 Gbps",
        UsbSpeed::SuperPlus => "10 Gbps",
        UsbSpeed::Unknown => "unknown",
    }
}

fn vid_pid(vendor_id: u16, product_id: u16) -> String {
    format!("{:04x}:{:04x}", vendor_id, product_id)
}

// ---------------------------------------------------------------------------
// Device list output
// ---------------------------------------------------------------------------

pub fn print_devices(devices: &[DeviceInfo], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(devices).unwrap());
        }
        OutputFormat::Quiet => {
            for d in devices {
                println!("{}", d.bus_id);
            }
        }
        OutputFormat::Human => {
            if devices.is_empty() {
                println!("No devices found.");
                return;
            }
            println!(
                "{:<12} {:<10} {:<20} {:<20} {:<10} {:<6}",
                "BUS ID", "VID:PID", "MANUFACTURER", "PRODUCT", "SPEED", "BOUND"
            );
            println!("{}", "-".repeat(80));
            for d in devices {
                println!(
                    "{:<12} {:<10} {:<20} {:<20} {:<10} {:<6}",
                    d.bus_id,
                    vid_pid(d.vendor_id, d.product_id),
                    d.manufacturer.as_deref().unwrap_or("-"),
                    d.product.as_deref().unwrap_or("-"),
                    speed_str(&d.speed),
                    if d.is_bound { "yes" } else { "no" },
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Exported devices output
// ---------------------------------------------------------------------------

pub fn print_exported_devices(devices: &[ExportedDeviceInfo], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(devices).unwrap());
        }
        OutputFormat::Quiet => {
            for d in devices {
                println!("{}", d.bus_id);
            }
        }
        OutputFormat::Human => {
            if devices.is_empty() {
                println!("No exported devices.");
                return;
            }
            println!(
                "{:<12} {:<10} {:<20} {:<20} {:<10} {:<8}",
                "BUS ID", "VID:PID", "MANUFACTURER", "PRODUCT", "SPEED", "CLIENTS"
            );
            println!("{}", "-".repeat(82));
            for d in devices {
                println!(
                    "{:<12} {:<10} {:<20} {:<20} {:<10} {:<8}",
                    d.bus_id,
                    vid_pid(d.vendor_id, d.product_id),
                    d.manufacturer.as_deref().unwrap_or("-"),
                    d.product.as_deref().unwrap_or("-"),
                    speed_str(&d.speed),
                    d.num_clients,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Imported devices output
// ---------------------------------------------------------------------------

pub fn print_imported_devices(devices: &[ImportedDeviceInfo], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(devices).unwrap());
        }
        OutputFormat::Quiet => {
            for d in devices {
                println!("{}", d.port);
            }
        }
        OutputFormat::Human => {
            if devices.is_empty() {
                println!("No imported devices.");
                return;
            }
            println!(
                "{:<6} {:<20} {:<12} {:<10} {:<10}",
                "PORT", "HOST", "REMOTE BUS", "VID:PID", "SPEED"
            );
            println!("{}", "-".repeat(60));
            for d in devices {
                println!(
                    "{:<6} {:<20} {:<12} {:<10} {:<10}",
                    d.port,
                    d.host,
                    d.remote_bus_id,
                    vid_pid(d.vendor_id, d.product_id),
                    speed_str(&d.speed),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Status output
// ---------------------------------------------------------------------------

pub fn print_status(
    status: &DaemonStatus,
    exported: &[ExportedDeviceInfo],
    imported: &[ImportedDeviceInfo],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let combined = serde_json::json!({
                "status": status,
                "exported_devices": exported,
                "imported_devices": imported,
            });
            println!("{}", serde_json::to_string_pretty(&combined).unwrap());
        }
        OutputFormat::Quiet => {
            println!(
                "exported={} imported={} connections={}",
                status.exported_devices, status.imported_devices, status.active_connections
            );
        }
        OutputFormat::Human => {
            println!("Daemon Status");
            println!("{}", "=".repeat(40));
            println!("Version:            {}", status.version);
            println!("Uptime:             {}s", status.uptime_secs);
            println!("Exported devices:   {}", status.exported_devices);
            println!("Imported devices:   {}", status.imported_devices);
            println!("Active connections: {}", status.active_connections);
            println!();

            println!("Exported Devices");
            println!("{}", "-".repeat(40));
            print_exported_devices(exported, OutputFormat::Human);
            println!();

            println!("Imported Devices");
            println!("{}", "-".repeat(40));
            print_imported_devices(imported, OutputFormat::Human);
        }
    }
}

// ---------------------------------------------------------------------------
// Simple message output
// ---------------------------------------------------------------------------

pub fn print_ok(msg: &str, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::json!({"status": "ok", "message": msg}));
        }
        OutputFormat::Quiet => {}
        OutputFormat::Human => {
            println!("{}", msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_device() -> DeviceInfo {
        DeviceInfo {
            bus_id: "1-1".to_string(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            manufacturer: Some("Acme".to_string()),
            product: Some("Widget".to_string()),
            device_class: 0,
            speed: UsbSpeed::High,
            is_bound: false,
        }
    }

    fn sample_exported() -> ExportedDeviceInfo {
        ExportedDeviceInfo {
            bus_id: "1-1".to_string(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            manufacturer: Some("Acme".to_string()),
            product: Some("Widget".to_string()),
            device_class: 0,
            speed: UsbSpeed::High,
            num_clients: 2,
        }
    }

    fn sample_imported() -> ImportedDeviceInfo {
        ImportedDeviceInfo {
            port: 0,
            host: "192.168.1.10".to_string(),
            remote_bus_id: "2-3".to_string(),
            vendor_id: 0xabcd,
            product_id: 0xef01,
            speed: UsbSpeed::Super,
        }
    }

    #[test]
    fn test_speed_str_values() {
        assert_eq!(speed_str(&UsbSpeed::Low), "1.5 Mbps");
        assert_eq!(speed_str(&UsbSpeed::Full), "12 Mbps");
        assert_eq!(speed_str(&UsbSpeed::High), "480 Mbps");
        assert_eq!(speed_str(&UsbSpeed::Super), "5 Gbps");
        assert_eq!(speed_str(&UsbSpeed::SuperPlus), "10 Gbps");
        assert_eq!(speed_str(&UsbSpeed::Unknown), "unknown");
    }

    #[test]
    fn test_vid_pid_format() {
        assert_eq!(vid_pid(0x1234, 0x5678), "1234:5678");
        assert_eq!(vid_pid(0x0001, 0x0002), "0001:0002");
    }

    #[test]
    fn test_print_devices_json() {
        let devices = vec![sample_device()];
        // Just ensure it doesn't panic; actual output goes to stdout.
        print_devices(&devices, OutputFormat::Json);
    }

    #[test]
    fn test_print_devices_quiet() {
        let devices = vec![sample_device()];
        print_devices(&devices, OutputFormat::Quiet);
    }

    #[test]
    fn test_print_devices_human() {
        let devices = vec![sample_device()];
        print_devices(&devices, OutputFormat::Human);
    }

    #[test]
    fn test_print_devices_empty() {
        print_devices(&[], OutputFormat::Human);
    }

    #[test]
    fn test_print_exported_devices() {
        let devices = vec![sample_exported()];
        print_exported_devices(&devices, OutputFormat::Human);
        print_exported_devices(&devices, OutputFormat::Json);
        print_exported_devices(&devices, OutputFormat::Quiet);
    }

    #[test]
    fn test_print_imported_devices() {
        let devices = vec![sample_imported()];
        print_imported_devices(&devices, OutputFormat::Human);
        print_imported_devices(&devices, OutputFormat::Json);
        print_imported_devices(&devices, OutputFormat::Quiet);
    }

    #[test]
    fn test_print_status() {
        let status = DaemonStatus {
            version: "0.1.0".to_string(),
            uptime_secs: 3600,
            exported_devices: 1,
            imported_devices: 1,
            active_connections: 2,
        };
        print_status(
            &status,
            &[sample_exported()],
            &[sample_imported()],
            OutputFormat::Human,
        );
        print_status(
            &status,
            &[sample_exported()],
            &[sample_imported()],
            OutputFormat::Json,
        );
        print_status(
            &status,
            &[sample_exported()],
            &[sample_imported()],
            OutputFormat::Quiet,
        );
    }

    #[test]
    fn test_print_ok() {
        print_ok("Device bound successfully.", OutputFormat::Human);
        print_ok("Device bound successfully.", OutputFormat::Json);
        print_ok("Device bound successfully.", OutputFormat::Quiet);
    }
}
