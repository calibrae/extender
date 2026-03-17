//! `extender discover` subcommand -- scan the LAN for Extender servers via mDNS.

use std::time::Duration;

use crate::output::OutputFormat;

/// Default discovery timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 3;

/// Execute the discover command.
pub async fn run(
    format: OutputFormat,
    timeout_secs: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

    if matches!(format, OutputFormat::Human) {
        eprintln!(
            "Scanning for Extender servers ({} seconds)...",
            timeout.as_secs()
        );
    }

    let servers = extender_client::discover::discover_servers(timeout).await;

    match format {
        OutputFormat::Json => {
            let json_servers: Vec<serde_json::Value> = servers
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "hostname": s.hostname,
                        "address": s.addr.ip().to_string(),
                        "port": s.addr.port(),
                        "version": s.version,
                        "device_count": s.device_count,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_servers)?);
        }
        OutputFormat::Quiet => {
            for s in &servers {
                println!("{}:{}", s.addr.ip(), s.addr.port());
            }
        }
        OutputFormat::Human => {
            if servers.is_empty() {
                println!("No Extender servers found.");
            } else {
                println!(
                    "{:<30} {:<20} {:<6} {:<10} {:<8}",
                    "HOSTNAME", "ADDRESS", "PORT", "VERSION", "DEVICES"
                );
                println!("{}", "-".repeat(76));
                for s in &servers {
                    println!(
                        "{:<30} {:<20} {:<6} {:<10} {:<8}",
                        s.hostname,
                        s.addr.ip(),
                        s.addr.port(),
                        s.version,
                        s.device_count,
                    );
                }
            }
        }
    }

    Ok(())
}
