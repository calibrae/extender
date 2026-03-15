//! Per-device import session: URB forwarding loop.
//!
//! A [`DeviceSession`] manages the URB phase for a single imported device.
//! It reads CmdSubmit/CmdUnlink messages from the TCP stream, dispatches
//! USB transfers via the managed device handle, and writes back the results.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use extender_protocol::codec::{read_urb_message, write_urb_message};
use extender_protocol::{
    CmdSubmit, CmdUnlink, Command, RetSubmit, RetUnlink, UrbMessage, UsbipHeaderBasic, ECONNRESET,
};

use crate::error::ServerError;
use crate::handle::ManagedDevice;
use crate::transfer::{execute_bulk_transfer, execute_control_transfer};

/// Default USB transfer timeout.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

/// Manages the URB forwarding loop for one imported device.
///
/// Reads CmdSubmit and CmdUnlink messages from the TCP stream,
/// dispatches USB transfers on the managed device, and writes
/// RetSubmit / RetUnlink responses back.
pub struct DeviceSession<S> {
    stream: Arc<Mutex<S>>,
    handle: Arc<ManagedDevice>,
    in_flight: Arc<Mutex<HashMap<u32, JoinHandle<()>>>>,
    bus_id: String,
}

impl<S> DeviceSession<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Create a new device session.
    pub fn new(stream: S, handle: Arc<ManagedDevice>, bus_id: String) -> Self {
        DeviceSession {
            stream: Arc::new(Mutex::new(stream)),
            handle,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            bus_id,
        }
    }

    /// Run the URB forwarding loop until the connection closes or an error occurs.
    ///
    /// This reads URB messages from the stream, dispatches them, and writes
    /// responses. CmdSubmit transfers are executed on a blocking thread pool
    /// to avoid stalling the async runtime.
    pub async fn run(self) -> Result<(), ServerError> {
        let stream = self.stream;
        let handle = self.handle;
        let in_flight = self.in_flight;
        let bus_id = self.bus_id;

        loop {
            // Read the next URB message from the stream.
            let msg = {
                let mut s = stream.lock().await;
                match read_urb_message(&mut *s).await {
                    Ok(msg) => msg,
                    Err(extender_protocol::ProtocolError::Io(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        tracing::info!(bus_id = %bus_id, "client disconnected (EOF)");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(bus_id = %bus_id, "failed to read URB message: {}", e);
                        return Err(ServerError::Protocol(e));
                    }
                }
            };

            match msg {
                UrbMessage::CmdSubmit(cmd) => {
                    let seqnum = cmd.header.seqnum;
                    let handle = Arc::clone(&handle);
                    let stream = Arc::clone(&stream);
                    let in_flight = Arc::clone(&in_flight);
                    let bus_id = bus_id.clone();

                    let in_flight_inner = Arc::clone(&in_flight);
                    let task = tokio::spawn(async move {
                        let result = handle_submit(cmd, &handle, &bus_id).await;
                        // Write response
                        {
                            let mut s = stream.lock().await;
                            if let Err(e) = write_urb_message(&mut *s, &result).await {
                                tracing::error!(
                                    bus_id = %bus_id,
                                    seqnum,
                                    "failed to write RetSubmit: {}", e
                                );
                            }
                        }
                        // Remove from in-flight.
                        in_flight_inner.lock().await.remove(&seqnum);
                    });

                    in_flight.lock().await.insert(seqnum, task);
                }
                UrbMessage::CmdUnlink(cmd) => {
                    let result = handle_unlink(&cmd, &in_flight).await;
                    let mut s = stream.lock().await;
                    if let Err(e) = write_urb_message(&mut *s, &result).await {
                        tracing::error!(
                            bus_id = %bus_id,
                            "failed to write RetUnlink: {}", e
                        );
                        break;
                    }
                }
                other => {
                    tracing::warn!(bus_id = %bus_id, "unexpected URB message: {:?}", other);
                }
            }
        }

        // Cancel all in-flight transfers.
        cancel_all_in_flight(&in_flight).await;
        Ok(())
    }
}

/// Execute a CmdSubmit by performing the USB transfer and building a RetSubmit.
async fn handle_submit(cmd: CmdSubmit, handle: &Arc<ManagedDevice>, bus_id: &str) -> UrbMessage {
    let seqnum = cmd.header.seqnum;
    let direction = cmd.header.direction;
    let ep = cmd.header.ep;
    let devid = cmd.header.devid;

    tracing::debug!(
        bus_id,
        seqnum,
        ep,
        direction,
        len = cmd.transfer_buffer_length,
        "processing CmdSubmit"
    );

    // Determine transfer type from endpoint number.
    // EP 0 = control, others = bulk or interrupt.
    // In USB/IP, we determine the type from the endpoint; for simplicity
    // we treat EP 0 as control and everything else as bulk.
    // Interrupt transfers would need endpoint descriptor info which
    // we don't have in the URB header -- the Linux kernel's USB/IP
    // implementation uses bulk for non-control non-iso endpoints.
    let handle_clone = Arc::clone(handle);
    let setup = cmd.setup;
    let data = cmd.transfer_buffer.clone();
    let buffer_length = cmd.transfer_buffer_length as usize;

    let result = if ep == 0 {
        // Control transfer on endpoint 0.
        tokio::task::spawn_blocking(move || {
            execute_control_transfer(&handle_clone, &setup, &data, TRANSFER_TIMEOUT)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(bus_id, seqnum, "spawn_blocking panicked: {}", e);
            crate::transfer::TransferResult {
                status: -5, // EIO
                actual_length: 0,
                data: Vec::new(),
            }
        })
    } else {
        // Determine the full endpoint address.
        let endpoint = if direction == 1 {
            ep as u8 | 0x80 // IN endpoint
        } else {
            ep as u8 // OUT endpoint
        };

        // Use bulk transfer for non-control endpoints.
        // (Interrupt endpoints also work via bulk at the libusb level for our use case.)
        let data_vec = data.to_vec();
        tokio::task::spawn_blocking(move || {
            execute_bulk_transfer(
                &handle_clone,
                endpoint,
                &data_vec,
                buffer_length,
                TRANSFER_TIMEOUT,
            )
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(bus_id, seqnum, "spawn_blocking panicked: {}", e);
            crate::transfer::TransferResult {
                status: -5,
                actual_length: 0,
                data: Vec::new(),
            }
        })
    };

    let transfer_buffer = if direction == 1 && !result.data.is_empty() {
        Bytes::from(result.data)
    } else {
        Bytes::new()
    };

    UrbMessage::RetSubmit(RetSubmit {
        header: UsbipHeaderBasic {
            command: Command::RetSubmit as u32,
            seqnum,
            devid,
            direction,
            ep,
        },
        status: result.status,
        actual_length: result.actual_length,
        start_frame: 0,
        number_of_packets: 0xFFFF_FFFF,
        error_count: 0,
        transfer_buffer,
    })
}

/// Handle a CmdUnlink by cancelling the in-flight URB if possible.
async fn handle_unlink(
    cmd: &CmdUnlink,
    in_flight: &Arc<Mutex<HashMap<u32, JoinHandle<()>>>>,
) -> UrbMessage {
    let target_seqnum = cmd.unlink_seqnum;
    let mut map = in_flight.lock().await;

    let status = if let Some(task) = map.remove(&target_seqnum) {
        // Abort the spawned task. The actual USB transfer may or may not
        // be cancellable at the libusb level, but we cancel the async wrapper.
        task.abort();
        tracing::debug!(
            seqnum = cmd.header.seqnum,
            target_seqnum,
            "unlinked in-flight URB"
        );
        ECONNRESET
    } else {
        // URB already completed or was never submitted.
        tracing::debug!(
            seqnum = cmd.header.seqnum,
            target_seqnum,
            "unlink target not found (already completed?)"
        );
        0
    };

    UrbMessage::RetUnlink(RetUnlink {
        header: UsbipHeaderBasic {
            command: Command::RetUnlink as u32,
            seqnum: cmd.header.seqnum,
            devid: cmd.header.devid,
            direction: cmd.header.direction,
            ep: cmd.header.ep,
        },
        status,
    })
}

/// Cancel all in-flight transfers, used during connection cleanup.
async fn cancel_all_in_flight(in_flight: &Arc<Mutex<HashMap<u32, JoinHandle<()>>>>) {
    let mut map = in_flight.lock().await;
    let count = map.len();
    for (seqnum, task) in map.drain() {
        task.abort();
        tracing::debug!(seqnum, "cancelled in-flight URB during cleanup");
    }
    if count > 0 {
        tracing::info!(count, "cancelled all in-flight URBs");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extender_protocol::{Command, UsbipHeaderBasic};

    #[tokio::test]
    async fn test_handle_unlink_not_found() {
        let in_flight: Arc<Mutex<HashMap<u32, JoinHandle<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let cmd = CmdUnlink {
            header: UsbipHeaderBasic {
                command: Command::CmdUnlink as u32,
                seqnum: 5,
                devid: 1,
                direction: 0,
                ep: 0,
            },
            unlink_seqnum: 3,
        };

        let result = handle_unlink(&cmd, &in_flight).await;
        match result {
            UrbMessage::RetUnlink(ret) => {
                assert_eq!(ret.header.seqnum, 5);
                assert_eq!(ret.status, 0); // Not found = already completed
            }
            _ => panic!("expected RetUnlink"),
        }
    }

    #[tokio::test]
    async fn test_handle_unlink_found() {
        let in_flight: Arc<Mutex<HashMap<u32, JoinHandle<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Insert a dummy in-flight task.
        let task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        in_flight.lock().await.insert(3, task);

        let cmd = CmdUnlink {
            header: UsbipHeaderBasic {
                command: Command::CmdUnlink as u32,
                seqnum: 5,
                devid: 1,
                direction: 0,
                ep: 0,
            },
            unlink_seqnum: 3,
        };

        let result = handle_unlink(&cmd, &in_flight).await;
        match result {
            UrbMessage::RetUnlink(ret) => {
                assert_eq!(ret.header.seqnum, 5);
                assert_eq!(ret.status, ECONNRESET);
            }
            _ => panic!("expected RetUnlink"),
        }

        // Verify it was removed from in-flight.
        assert!(in_flight.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_all_in_flight() {
        let in_flight: Arc<Mutex<HashMap<u32, JoinHandle<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        for i in 1..=3 {
            let task = tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
            in_flight.lock().await.insert(i, task);
        }

        cancel_all_in_flight(&in_flight).await;
        assert!(in_flight.lock().await.is_empty());
    }
}
