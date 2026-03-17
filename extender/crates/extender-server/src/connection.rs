//! Per-connection handler for the USB/IP server.
//!
//! Each inbound TCP connection is handled here. The handler reads the
//! first discovery-phase message to determine whether the client wants
//! a device list or an import, then dispatches accordingly.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;

use extender_protocol::codec::{read_op_message, write_op_message};
use extender_protocol::{OpMessage, OpRepDevlist, OpRepImport};

use crate::error::ServerError;
use crate::export::ExportRegistry;
use crate::session::DeviceSession;

/// Handle one inbound TCP connection.
///
/// Reads the first discovery-phase message and dispatches to the
/// appropriate handler:
/// - `OP_REQ_DEVLIST` -> list exported devices, close connection
/// - `OP_REQ_IMPORT` -> import a device, enter URB forwarding loop
pub async fn handle_connection(
    mut stream: TcpStream,
    registry: Arc<ExportRegistry>,
    peer: std::net::SocketAddr,
) {
    tracing::info!(%peer, "new connection");

    let msg =
        match tokio::time::timeout(Duration::from_secs(10), read_op_message(&mut stream)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                tracing::warn!(%peer, "failed to read initial message: {}", e);
                return;
            }
            Err(_) => {
                tracing::warn!(%peer, "timed out waiting for initial message");
                return;
            }
        };

    match msg {
        OpMessage::ReqDevlist(_) => {
            tracing::debug!(%peer, "handling DEVLIST request");
            if let Err(e) = handle_devlist(&mut stream, &registry).await {
                tracing::warn!(%peer, "DEVLIST handler error: {}", e);
            }
            // Connection closes after DEVLIST response.
        }
        OpMessage::ReqImport(req) => {
            let bus_id = extract_bus_id(&req.busid);
            if !is_valid_bus_id(&bus_id) {
                tracing::warn!(%peer, bus_id, "invalid bus ID format, rejecting import");
                let reply = OpMessage::RepImport(Box::new(OpRepImport {
                    status: 1,
                    device: None,
                }));
                let _ = write_op_message(&mut stream, &reply).await;
                return;
            }
            tracing::debug!(%peer, bus_id, "handling IMPORT request");
            match handle_import(&mut stream, &registry, &bus_id).await {
                Ok(Some((handle, session_id))) => {
                    // Successfully imported -- enter URB forwarding loop.
                    tracing::info!(%peer, bus_id, session_id, "entering URB forwarding loop");
                    let (reader, writer) = tokio::io::split(stream);
                    let session = DeviceSession::new(reader, writer, handle, bus_id.to_string());
                    if let Err(e) = session.run().await {
                        tracing::warn!(%peer, bus_id, "URB session error: {}", e);
                    }
                    // Release the device when the session ends.
                    registry.release(&bus_id, session_id).await;
                    tracing::info!(%peer, bus_id, session_id, "session ended, device released");
                }
                Ok(None) => {
                    // Import was rejected; connection closes.
                    tracing::info!(%peer, bus_id, "import rejected");
                }
                Err(e) => {
                    tracing::warn!(%peer, bus_id, "IMPORT handler error: {}", e);
                }
            }
        }
        other => {
            tracing::warn!(%peer, "unexpected initial message: {:?}", other);
        }
    }
}

/// Handle an OP_REQ_DEVLIST: enumerate the export registry and send back
/// the device list.
async fn handle_devlist(
    stream: &mut TcpStream,
    registry: &ExportRegistry,
) -> Result<(), ServerError> {
    let devices = registry.list_devices().await?;

    let reply = OpMessage::RepDevlist(OpRepDevlist { status: 0, devices });

    write_op_message(stream, &reply)
        .await
        .map_err(ServerError::Protocol)?;

    Ok(())
}

/// Handle an OP_REQ_IMPORT: try to acquire the device from the registry.
///
/// Returns `Ok(Some((handle, session_id)))` if the import succeeded and the
/// connection should continue into the URB phase. Returns `Ok(None)` if the
/// import was rejected (error reply sent to client). Returns `Err` on I/O errors.
async fn handle_import(
    stream: &mut TcpStream,
    registry: &ExportRegistry,
    bus_id: &str,
) -> Result<Option<(Arc<crate::handle::ManagedDevice>, crate::export::SessionId)>, ServerError> {
    match registry.try_acquire(bus_id).await {
        Ok((handle, proto_device, session_id)) => {
            let reply = OpMessage::RepImport(Box::new(OpRepImport {
                status: 0,
                device: Some(proto_device),
            }));
            write_op_message(stream, &reply)
                .await
                .map_err(ServerError::Protocol)?;
            Ok(Some((handle, session_id)))
        }
        Err(_) => {
            // Device not found, not bound, or already in use.
            let reply = OpMessage::RepImport(Box::new(OpRepImport {
                status: 1,
                device: None,
            }));
            write_op_message(stream, &reply)
                .await
                .map_err(ServerError::Protocol)?;
            Ok(None)
        }
    }
}

/// Extract a bus ID string from the null-padded 32-byte busid field.
fn extract_bus_id(busid: &[u8; 32]) -> String {
    let end = busid.iter().position(|&b| b == 0).unwrap_or(32);
    String::from_utf8_lossy(&busid[..end]).to_string()
}

/// Validate that a bus ID matches the expected pattern `[0-9]+-[0-9]+(\.[0-9]+)*`.
///
/// Examples of valid bus IDs: "1-1", "2-4", "1-4.2", "3-1.2.3".
fn is_valid_bus_id(bus_id: &str) -> bool {
    // Split on '-'; must have exactly two parts.
    let mut parts = bus_id.splitn(2, '-');
    let bus_part = match parts.next() {
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => s,
        _ => return false,
    };
    let dev_part = match parts.next() {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };
    // bus_part must be all digits (already checked above).
    let _ = bus_part;
    // dev_part must be dot-separated groups of digits, each non-empty.
    for segment in dev_part.split('.') {
        if segment.is_empty() || !segment.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_bus_id() {
        let mut busid = [0u8; 32];
        busid[..5].copy_from_slice(b"1-4.2");
        assert_eq!(extract_bus_id(&busid), "1-4.2");
    }

    #[test]
    fn test_extract_bus_id_full() {
        let busid = [b'a'; 32];
        assert_eq!(extract_bus_id(&busid).len(), 32);
    }

    #[test]
    fn test_extract_bus_id_empty() {
        let busid = [0u8; 32];
        assert_eq!(extract_bus_id(&busid), "");
    }

    #[test]
    fn test_valid_bus_ids() {
        assert!(is_valid_bus_id("1-1"));
        assert!(is_valid_bus_id("2-4"));
        assert!(is_valid_bus_id("1-4.2"));
        assert!(is_valid_bus_id("3-1.2.3"));
        assert!(is_valid_bus_id("10-12.3.45"));
    }

    #[test]
    fn test_invalid_bus_ids() {
        assert!(!is_valid_bus_id(""));
        assert!(!is_valid_bus_id("abc"));
        assert!(!is_valid_bus_id("1-"));
        assert!(!is_valid_bus_id("-1"));
        assert!(!is_valid_bus_id("1-1."));
        assert!(!is_valid_bus_id("1-1..2"));
        assert!(!is_valid_bus_id("a-1"));
        assert!(!is_valid_bus_id("1-a"));
        assert!(!is_valid_bus_id("../etc/passwd"));
        assert!(!is_valid_bus_id("1-1; rm -rf /"));
    }
}
