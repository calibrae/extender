//! Unix domain socket JSON-RPC API server.

use std::sync::Arc;

use extender_api::{
    read_message, write_message, DaemonStatus, DeviceInfo, ExportedDeviceInfo, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, UsbSpeed,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument};

/// Shared state accessible by all API connection handlers.
pub struct ApiState {
    pub start_time: std::time::Instant,
    pub event_tx: broadcast::Sender<String>,
}

impl Default for ApiState {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiState {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            start_time: std::time::Instant::now(),
            event_tx,
        }
    }
}

/// Start the API server on a Unix domain socket.
///
/// Listens for incoming connections and spawns a task per client.
/// Returns when the `shutdown` token is cancelled.
pub async fn run_api_server(
    socket_path: &str,
    shutdown: CancellationToken,
    state: Arc<ApiState>,
) -> std::io::Result<()> {
    // Remove stale socket file if present.
    if std::path::Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;

    // Set socket permissions to 0o660 on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o660);
        std::fs::set_permissions(socket_path, perms)?;
    }

    info!("API server listening on {}", socket_path);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let state = Arc::clone(&state);
                        let shutdown = shutdown.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(stream, state, shutdown).await {
                                debug!("client connection ended: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("failed to accept connection: {}", e);
                    }
                }
            }
            _ = shutdown.cancelled() => {
                info!("API server shutting down");
                break;
            }
        }
    }

    // Clean up socket file.
    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// Handle a single client connection: read requests, dispatch, write responses.
#[instrument(skip_all)]
async fn handle_client(
    stream: UnixStream,
    state: Arc<ApiState>,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = stream.into_split();
    let mut event_rx: Option<broadcast::Receiver<String>> = None;

    loop {
        tokio::select! {
            msg = read_message(&mut reader) => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(extender_api::FramingError::ConnectionClosed) => {
                        debug!("client disconnected");
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                };

                let request: JsonRpcRequest = match serde_json::from_slice(&msg) {
                    Ok(r) => r,
                    Err(e) => {
                        let resp = JsonRpcResponse::error(
                            None,
                            JsonRpcError::parse_error(e.to_string()),
                        );
                        let resp_bytes = serde_json::to_vec(&resp)?;
                        write_message(&mut writer, &resp_bytes).await?;
                        continue;
                    }
                };

                debug!(method = %request.method, "dispatching API request");

                let response = dispatch(&request, &state, &mut event_rx).await;
                let resp_bytes = serde_json::to_vec(&response)?;
                write_message(&mut writer, &resp_bytes).await?;
            }
            // If subscribed, forward events as JSON-RPC notifications.
            notification = async {
                match &mut event_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Ok(event_json) = notification {
                    let notif = JsonRpcRequest::notification("event", Some(
                        serde_json::Value::String(event_json),
                    ));
                    let notif_bytes = serde_json::to_vec(&notif)?;
                    write_message(&mut writer, &notif_bytes).await?;
                }
            }
            _ = shutdown.cancelled() => {
                debug!("shutdown signalled, closing client connection");
                return Ok(());
            }
        }
    }
}

/// Dispatch a JSON-RPC request to the appropriate handler.
async fn dispatch(
    req: &JsonRpcRequest,
    state: &ApiState,
    event_rx: &mut Option<broadcast::Receiver<String>>,
) -> JsonRpcResponse {
    let result = match req.method.as_str() {
        "list_local_devices" => handle_list_local_devices(),
        "list_exported_devices" => handle_list_exported_devices(),
        "list_remote_devices" => handle_list_remote_devices(&req.params),
        "bind_device" => handle_bind_device(&req.params),
        "unbind_device" => handle_unbind_device(&req.params),
        "attach_device" => handle_attach_device(&req.params),
        "detach_device" => handle_detach_device(&req.params),
        "get_status" => handle_get_status(state),
        "get_device_info" => handle_get_device_info(&req.params),
        "subscribe" => handle_subscribe(&req.params, state, event_rx),
        _ => Err(JsonRpcError::method_not_found(&req.method)),
    };

    match result {
        Ok(value) => JsonRpcResponse::success(req.id.clone(), value),
        Err(error) => JsonRpcResponse::error(req.id.clone(), error),
    }
}

// ---------------------------------------------------------------------------
// Stub method handlers — return placeholder data for now.
// ---------------------------------------------------------------------------

fn handle_list_local_devices() -> Result<serde_json::Value, JsonRpcError> {
    let devices: Vec<DeviceInfo> = vec![];
    serde_json::to_value(&devices).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

fn handle_list_exported_devices() -> Result<serde_json::Value, JsonRpcError> {
    let devices: Vec<ExportedDeviceInfo> = vec![];
    serde_json::to_value(&devices).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

fn handle_list_remote_devices(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let _host = params
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'host'"))?;
    let _port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'port'"))?;

    // Stub: return empty list.
    let devices: Vec<DeviceInfo> = vec![];
    serde_json::to_value(&devices).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

fn handle_bind_device(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let _bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    Ok(serde_json::json!({"status": "ok"}))
}

fn handle_unbind_device(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let _bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    Ok(serde_json::json!({"status": "ok"}))
}

fn handle_attach_device(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let _host = params
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'host'"))?;
    let _port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'port'"))?;
    let _bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    Ok(serde_json::json!({"status": "ok"}))
}

fn handle_detach_device(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let _port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'port'"))?;

    Ok(serde_json::json!({"status": "ok"}))
}

fn handle_get_status(state: &ApiState) -> Result<serde_json::Value, JsonRpcError> {
    let status = DaemonStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        exported_devices: 0,
        imported_devices: 0,
        active_connections: 0,
    };
    serde_json::to_value(&status).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

fn handle_get_device_info(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    // Stub: return a placeholder device.
    let device = DeviceInfo {
        bus_id: bus_id.to_string(),
        vendor_id: 0,
        product_id: 0,
        manufacturer: None,
        product: None,
        device_class: 0,
        speed: UsbSpeed::Unknown,
        is_bound: false,
    };
    serde_json::to_value(&device).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

fn handle_subscribe(
    params: &Option<serde_json::Value>,
    state: &ApiState,
    event_rx: &mut Option<broadcast::Receiver<String>>,
) -> Result<serde_json::Value, JsonRpcError> {
    let _events: Vec<String> = match params {
        Some(p) => p
            .get("events")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default(),
        None => vec![],
    };

    *event_rx = Some(state.event_tx.subscribe());
    info!("client subscribed to events");

    Ok(serde_json::json!({"status": "ok"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use extender_api::{read_message, write_message, JsonRpcRequest};
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn test_api_server_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let socket_str = socket_path.to_string_lossy().to_string();

        let shutdown = CancellationToken::new();
        let state = Arc::new(ApiState::new());

        let server_shutdown = shutdown.clone();
        let server_socket = socket_str.clone();
        let server_state = Arc::clone(&state);
        let server_handle = tokio::spawn(async move {
            run_api_server(&server_socket, server_shutdown, server_state)
                .await
                .unwrap();
        });

        // Give the server a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect as a client.
        let stream = UnixStream::connect(&socket_str).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();

        // Send a get_status request.
        let req = JsonRpcRequest::new("get_status", None, 1);
        let req_bytes = serde_json::to_vec(&req).unwrap();
        write_message(&mut writer, &req_bytes).await.unwrap();

        // Read the response.
        let resp_bytes = read_message(&mut reader).await.unwrap();
        let resp: JsonRpcResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert_eq!(resp.id, Some(serde_json::Value::Number(1.into())));

        // Test method_not_found.
        let req2 = JsonRpcRequest::new("nonexistent", None, 2);
        let req2_bytes = serde_json::to_vec(&req2).unwrap();
        write_message(&mut writer, &req2_bytes).await.unwrap();

        let resp2_bytes = read_message(&mut reader).await.unwrap();
        let resp2: JsonRpcResponse = serde_json::from_slice(&resp2_bytes).unwrap();
        assert!(resp2.error.is_some());
        assert_eq!(resp2.error.unwrap().code, -32601);

        // Test invalid JSON.
        write_message(&mut writer, b"not json at all")
            .await
            .unwrap();
        let resp3_bytes = read_message(&mut reader).await.unwrap();
        let resp3: JsonRpcResponse = serde_json::from_slice(&resp3_bytes).unwrap();
        assert!(resp3.error.is_some());
        assert_eq!(resp3.error.unwrap().code, -32700);

        // Shut down.
        shutdown.cancel();
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_bind_requires_params() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test2.sock");
        let socket_str = socket_path.to_string_lossy().to_string();

        let shutdown = CancellationToken::new();
        let state = Arc::new(ApiState::new());

        let server_shutdown = shutdown.clone();
        let server_socket = socket_str.clone();
        let server_state = Arc::clone(&state);
        tokio::spawn(async move {
            let _ = run_api_server(&server_socket, server_shutdown, server_state).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let stream = UnixStream::connect(&socket_str).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();

        // Send bind_device without params.
        let req = JsonRpcRequest::new("bind_device", None, 1);
        let req_bytes = serde_json::to_vec(&req).unwrap();
        write_message(&mut writer, &req_bytes).await.unwrap();

        let resp_bytes = read_message(&mut reader).await.unwrap();
        let resp: JsonRpcResponse = serde_json::from_slice(&resp_bytes).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);

        shutdown.cancel();
    }
}
