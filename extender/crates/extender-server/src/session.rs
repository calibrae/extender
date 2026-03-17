//! Per-device import session: URB forwarding loop.
//!
//! Uses split read/write halves with a channel so USB transfer responses
//! can be written concurrently with reading new requests.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use extender_protocol::codec::{read_urb_message, write_urb_message};
use extender_protocol::{
    CmdSubmit, CmdUnlink, Command, RetSubmit, RetUnlink, UrbMessage, UsbipHeaderBasic, ECONNRESET,
};

use crate::error::ServerError;
use crate::handle::ManagedDevice;
use crate::transfer::{execute_bulk_transfer, execute_control_transfer};

/// Default USB transfer timeout.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// Channel capacity for response messages.
const RESPONSE_CHANNEL_SIZE: usize = 64;

/// Thread-safe tracker for the last successful URB timestamp.
///
/// Updated each time a URB response is successfully sent. Can be used by
/// external monitoring to detect stale sessions.
#[derive(Debug, Clone)]
pub struct SessionHealth {
    last_urb_at: Arc<Mutex<Option<tokio::time::Instant>>>,
}

impl SessionHealth {
    /// Create a new health tracker.
    pub fn new() -> Self {
        SessionHealth {
            last_urb_at: Arc::new(Mutex::new(None)),
        }
    }

    /// Record a successful URB response.
    pub async fn record_urb(&self) {
        *self.last_urb_at.lock().await = Some(tokio::time::Instant::now());
    }

    /// Get the last successful URB timestamp.
    pub async fn last_urb_time(&self) -> Option<tokio::time::Instant> {
        *self.last_urb_at.lock().await
    }

    /// Check how long since the last URB activity.
    pub async fn idle_duration(&self) -> Option<Duration> {
        self.last_urb_at.lock().await.map(|t| t.elapsed())
    }
}

impl Default for SessionHealth {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages the URB forwarding loop for one imported device.
pub struct DeviceSession<R, W> {
    reader: R,
    writer: W,
    handle: Arc<ManagedDevice>,
    in_flight: Arc<Mutex<HashMap<u32, JoinHandle<()>>>>,
    bus_id: String,
    health: SessionHealth,
}

impl<R, W> DeviceSession<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    /// Create a new device session from split read/write halves.
    pub fn new(reader: R, writer: W, handle: Arc<ManagedDevice>, bus_id: String) -> Self {
        DeviceSession {
            reader,
            writer,
            handle,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            bus_id,
            health: SessionHealth::new(),
        }
    }

    /// Get a reference to the session health tracker.
    pub fn health(&self) -> &SessionHealth {
        &self.health
    }

    /// Run the URB forwarding loop.
    ///
    /// Spawns a writer task that drains a response channel, then reads
    /// URB messages in a loop, dispatching USB transfers that send
    /// their responses through the channel. This allows concurrent
    /// reads and writes on the TCP stream.
    pub async fn run(self) -> Result<(), ServerError> {
        let mut reader = self.reader;
        let mut writer = self.writer;
        let handle = self.handle;
        let in_flight = self.in_flight;
        let bus_id = self.bus_id;
        let health = self.health;

        // Channel for responses from transfer tasks → writer task.
        let (tx, mut rx) = mpsc::channel::<UrbMessage>(RESPONSE_CHANNEL_SIZE);

        // Writer task: drains the channel and writes to the TCP stream.
        let writer_health = health.clone();
        let writer_handle = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = write_urb_message(&mut writer, &msg).await {
                    tracing::error!("failed to write URB response: {}", e);
                    break;
                }
                writer_health.record_urb().await;
            }
        });

        // Reader loop: reads URB messages and dispatches them.
        loop {
            let msg = match read_urb_message(&mut reader).await {
                Ok(msg) => msg,
                Err(extender_protocol::ProtocolError::Io(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    tracing::info!(bus_id = %bus_id, "client disconnected (EOF)");
                    break;
                }
                Err(e) => {
                    tracing::error!(bus_id = %bus_id, "failed to read URB message: {}", e);
                    break;
                }
            };

            match msg {
                UrbMessage::CmdSubmit(cmd) => {
                    let seqnum = cmd.header.seqnum;
                    let handle = Arc::clone(&handle);
                    let in_flight_clone = Arc::clone(&in_flight);
                    let bus_id = bus_id.clone();
                    let tx = tx.clone();

                    let task = tokio::spawn(async move {
                        let result = handle_submit(cmd, &handle, &bus_id).await;
                        // Send response through channel (non-blocking).
                        let _ = tx.send(result).await;
                        // Remove from in-flight.
                        in_flight_clone.lock().await.remove(&seqnum);
                    });

                    in_flight.lock().await.insert(seqnum, task);
                }
                UrbMessage::CmdUnlink(cmd) => {
                    let result = handle_unlink(&cmd, &in_flight).await;
                    // Send unlink response through channel.
                    if tx.send(result).await.is_err() {
                        tracing::error!(bus_id = %bus_id, "writer channel closed");
                        break;
                    }
                }
                other => {
                    tracing::warn!(bus_id = %bus_id, "unexpected URB message: {:?}", other);
                }
            }
        }

        // Cleanup: cancel in-flight, drop sender to close writer task.
        cancel_all_in_flight(&in_flight).await;
        drop(tx);
        let _ = writer_handle.await;
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

    let handle_clone = Arc::clone(handle);
    let setup = cmd.setup;
    let data = cmd.transfer_buffer.clone();
    let buffer_length = cmd.transfer_buffer_length as usize;

    let result = if ep == 0 {
        tracing::debug!(
            bus_id,
            seqnum,
            "control: bmReqType=0x{:02x} bReq=0x{:02x} wVal=0x{:04x} wIdx=0x{:04x} wLen={}",
            setup[0],
            setup[1],
            u16::from_le_bytes([setup[2], setup[3]]),
            u16::from_le_bytes([setup[4], setup[5]]),
            u16::from_le_bytes([setup[6], setup[7]]),
        );
        tokio::task::spawn_blocking(move || {
            execute_control_transfer(&handle_clone, &setup, &data, TRANSFER_TIMEOUT)
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
    } else {
        let endpoint = if direction == 1 {
            ep as u8 | 0x80
        } else {
            ep as u8
        };
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

    tracing::debug!(
        bus_id,
        seqnum,
        ep,
        status = result.status,
        actual_length = result.actual_length,
        data_len = result.data.len(),
        "transfer complete"
    );

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
        task.abort();
        tracing::debug!(
            seqnum = cmd.header.seqnum,
            target_seqnum,
            "unlinked in-flight URB"
        );
        ECONNRESET
    } else {
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

/// Cancel all in-flight transfers.
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
                assert_eq!(ret.status, 0);
            }
            _ => panic!("expected RetUnlink"),
        }
    }

    #[tokio::test]
    async fn test_handle_unlink_found() {
        let in_flight: Arc<Mutex<HashMap<u32, JoinHandle<()>>>> =
            Arc::new(Mutex::new(HashMap::new()));

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
