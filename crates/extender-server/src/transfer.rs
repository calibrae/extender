//! USB transfer execution.
//!
//! Provides functions to execute control, bulk, and interrupt transfers
//! on a managed USB device, mapping rusb errors to USB/IP status codes
//! (Linux errno values).

use std::time::Duration;

use crate::error::rusb_error_to_errno;
use crate::handle::ManagedDevice;

/// Result of a USB transfer, containing the status code and any data.
#[derive(Debug)]
pub struct TransferResult {
    /// USB/IP status code (0 = success, negative = Linux errno).
    pub status: i32,
    /// Actual number of bytes transferred.
    pub actual_length: u32,
    /// Transfer buffer data (for IN transfers).
    pub data: Vec<u8>,
}

/// Direction of a USB control transfer, derived from the setup packet's
/// bmRequestType field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlDirection {
    /// Host to device (OUT).
    Out,
    /// Device to host (IN).
    In,
}

/// Parse the direction from a USB setup packet's bmRequestType byte.
fn control_direction(bm_request_type: u8) -> ControlDirection {
    if bm_request_type & 0x80 != 0 {
        ControlDirection::In
    } else {
        ControlDirection::Out
    }
}

/// Execute a USB control transfer.
///
/// The setup packet is the standard 8-byte USB setup packet:
/// - `setup[0]` = bmRequestType
/// - `setup[1]` = bRequest
/// - `setup[2..4]` = wValue (little-endian)
/// - `setup[4..6]` = wIndex (little-endian)
/// - `setup[6..8]` = wLength (little-endian)
///
/// For OUT transfers, `data` contains the data to send.
/// For IN transfers, `data` is ignored and the response data is returned
/// in `TransferResult::data`.
pub fn execute_control_transfer(
    device: &ManagedDevice,
    setup: &[u8; 8],
    data: &[u8],
    timeout: Duration,
) -> TransferResult {
    let bm_request_type = setup[0];
    let b_request = setup[1];
    let w_value = u16::from_le_bytes([setup[2], setup[3]]);
    let w_index = u16::from_le_bytes([setup[4], setup[5]]);
    let w_length = u16::from_le_bytes([setup[6], setup[7]]);

    let direction = control_direction(bm_request_type);
    let handle = device.handle();

    match direction {
        ControlDirection::Out => {
            match handle.write_control(bm_request_type, b_request, w_value, w_index, data, timeout)
            {
                Ok(len) => TransferResult {
                    status: 0,
                    actual_length: len as u32,
                    data: Vec::new(),
                },
                Err(e) => {
                    tracing::debug!(
                        bus_id = device.bus_id(),
                        "control OUT transfer failed: {}",
                        e
                    );
                    TransferResult {
                        status: rusb_error_to_errno(&e),
                        actual_length: 0,
                        data: Vec::new(),
                    }
                }
            }
        }
        ControlDirection::In => {
            let mut buf = vec![0u8; w_length as usize];
            match handle.read_control(
                bm_request_type,
                b_request,
                w_value,
                w_index,
                &mut buf,
                timeout,
            ) {
                Ok(len) => {
                    buf.truncate(len);
                    TransferResult {
                        status: 0,
                        actual_length: len as u32,
                        data: buf,
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        bus_id = device.bus_id(),
                        "control IN transfer failed: {}",
                        e
                    );
                    TransferResult {
                        status: rusb_error_to_errno(&e),
                        actual_length: 0,
                        data: Vec::new(),
                    }
                }
            }
        }
    }
}

/// Execute a USB bulk transfer.
///
/// - For OUT transfers (endpoint bit 7 = 0): sends `data` to the endpoint.
/// - For IN transfers (endpoint bit 7 = 1): reads up to `buffer_length` bytes.
pub fn execute_bulk_transfer(
    device: &ManagedDevice,
    endpoint: u8,
    data: &[u8],
    buffer_length: usize,
    timeout: Duration,
) -> TransferResult {
    let handle = device.handle();
    let is_in = endpoint & 0x80 != 0;

    if is_in {
        let mut buf = vec![0u8; buffer_length];
        match handle.read_bulk(endpoint, &mut buf, timeout) {
            Ok(len) => {
                buf.truncate(len);
                TransferResult {
                    status: 0,
                    actual_length: len as u32,
                    data: buf,
                }
            }
            Err(e) => {
                tracing::debug!(
                    bus_id = device.bus_id(),
                    endpoint,
                    "bulk IN transfer failed: {}",
                    e
                );
                TransferResult {
                    status: rusb_error_to_errno(&e),
                    actual_length: 0,
                    data: Vec::new(),
                }
            }
        }
    } else {
        match handle.write_bulk(endpoint, data, timeout) {
            Ok(len) => TransferResult {
                status: 0,
                actual_length: len as u32,
                data: Vec::new(),
            },
            Err(e) => {
                tracing::debug!(
                    bus_id = device.bus_id(),
                    endpoint,
                    "bulk OUT transfer failed: {}",
                    e
                );
                TransferResult {
                    status: rusb_error_to_errno(&e),
                    actual_length: 0,
                    data: Vec::new(),
                }
            }
        }
    }
}

/// Execute a USB interrupt transfer.
///
/// - For IN transfers (endpoint bit 7 = 1): reads up to `buffer_length` bytes.
/// - For OUT transfers (endpoint bit 7 = 0): sends `data` to the endpoint.
pub fn execute_interrupt_transfer(
    device: &ManagedDevice,
    endpoint: u8,
    data: &[u8],
    buffer_length: usize,
    timeout: Duration,
) -> TransferResult {
    let handle = device.handle();
    let is_in = endpoint & 0x80 != 0;

    if is_in {
        let mut buf = vec![0u8; buffer_length];
        match handle.read_interrupt(endpoint, &mut buf, timeout) {
            Ok(len) => {
                buf.truncate(len);
                TransferResult {
                    status: 0,
                    actual_length: len as u32,
                    data: buf,
                }
            }
            Err(e) => {
                tracing::debug!(
                    bus_id = device.bus_id(),
                    endpoint,
                    "interrupt IN transfer failed: {}",
                    e
                );
                TransferResult {
                    status: rusb_error_to_errno(&e),
                    actual_length: 0,
                    data: Vec::new(),
                }
            }
        }
    } else {
        match handle.write_interrupt(endpoint, data, timeout) {
            Ok(len) => TransferResult {
                status: 0,
                actual_length: len as u32,
                data: Vec::new(),
            },
            Err(e) => {
                tracing::debug!(
                    bus_id = device.bus_id(),
                    endpoint,
                    "interrupt OUT transfer failed: {}",
                    e
                );
                TransferResult {
                    status: rusb_error_to_errno(&e),
                    actual_length: 0,
                    data: Vec::new(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_direction_in() {
        assert_eq!(control_direction(0x80), ControlDirection::In);
        assert_eq!(control_direction(0xC0), ControlDirection::In);
        assert_eq!(control_direction(0xA1), ControlDirection::In);
    }

    #[test]
    fn test_control_direction_out() {
        assert_eq!(control_direction(0x00), ControlDirection::Out);
        assert_eq!(control_direction(0x40), ControlDirection::Out);
        assert_eq!(control_direction(0x21), ControlDirection::Out);
    }

    #[test]
    fn test_transfer_result_success() {
        let result = TransferResult {
            status: 0,
            actual_length: 64,
            data: vec![0; 64],
        };
        assert_eq!(result.status, 0);
        assert_eq!(result.actual_length, 64);
        assert_eq!(result.data.len(), 64);
    }

    #[test]
    fn test_transfer_result_error() {
        let result = TransferResult {
            status: -32, // EPIPE
            actual_length: 0,
            data: Vec::new(),
        };
        assert_eq!(result.status, -32);
    }
}
