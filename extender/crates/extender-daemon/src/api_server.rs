//! IPC API server (Unix domain socket on Unix, TCP localhost on Windows).

use std::net::SocketAddr;
use std::sync::Arc;

use crate::config::SecurityConfig;
use crate::device_acl;
use extender_api::{
    read_message, write_message, DaemonStatus, DeviceInfo, ExportedDeviceInfo, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, UsbSpeed,
};
use extender_server::export::ExportRegistry;
#[cfg(windows)]
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument};

/// Shared state accessible by all API connection handlers.
pub struct ApiState {
    pub start_time: std::time::Instant,
    pub event_tx: broadcast::Sender<String>,
    pub registry: Arc<ExportRegistry>,
    pub security: SecurityConfig,
}

impl ApiState {
    pub fn new(registry: Arc<ExportRegistry>, security: SecurityConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            start_time: std::time::Instant::now(),
            event_tx,
            registry,
            security,
        }
    }
}

/// Default TCP port used for the IPC transport on Windows.
#[cfg(windows)]
const DEFAULT_API_PORT: u16 = 9241;

/// Parse the API address from the socket_path configuration value.
///
/// On Unix this is a filesystem path for the Unix domain socket.
/// On Windows this is interpreted as a TCP port number (falling back to the
/// default port when parsing fails) and the server binds to `127.0.0.1`.
#[cfg(windows)]
fn parse_api_port(socket_path: &str) -> u16 {
    socket_path.parse::<u16>().unwrap_or(DEFAULT_API_PORT)
}

/// Start the API server on a Unix domain socket.
#[cfg(unix)]
pub async fn run_api_server(
    socket_path: &str,
    shutdown: CancellationToken,
    state: Arc<ApiState>,
) -> std::io::Result<()> {
    if std::path::Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;

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

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// Start the API server on a TCP socket bound to localhost (Windows).
#[cfg(windows)]
pub async fn run_api_server(
    socket_path: &str,
    shutdown: CancellationToken,
    state: Arc<ApiState>,
) -> std::io::Result<()> {
    let port = parse_api_port(socket_path);
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;

    info!("API server listening on {}", addr);

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

    Ok(())
}

#[cfg(unix)]
#[instrument(skip_all)]
async fn handle_client(
    stream: UnixStream,
    state: Arc<ApiState>,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = stream.into_split();
    handle_client_io(&mut reader, &mut writer, state, shutdown).await
}

#[cfg(windows)]
#[instrument(skip_all)]
async fn handle_client(
    stream: TcpStream,
    state: Arc<ApiState>,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = stream.into_split();
    handle_client_io(&mut reader, &mut writer, state, shutdown).await
}

#[instrument(skip_all)]
async fn handle_client_io(
    mut reader: &mut (impl tokio::io::AsyncRead + Unpin),
    mut writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    state: Arc<ApiState>,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
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
        "list_exported_devices" => handle_list_exported_devices(state).await,
        "list_remote_devices" => handle_list_remote_devices(&req.params).await,
        "bind_device" => handle_bind_device(&req.params, state).await,
        "unbind_device" => handle_unbind_device(&req.params, state).await,
        "attach_device" => handle_attach_device(&req.params).await,
        "detach_device" => handle_detach_device(&req.params).await,
        "get_status" => handle_get_status(state).await,
        "get_device_info" => handle_get_device_info(&req.params, state),
        "subscribe" => handle_subscribe(&req.params, state, event_rx),
        _ => Err(JsonRpcError::method_not_found(&req.method)),
    };

    match result {
        Ok(value) => JsonRpcResponse::success(req.id.clone(), value),
        Err(error) => JsonRpcResponse::error(req.id.clone(), error),
    }
}

// ---------------------------------------------------------------------------
// Real method handlers
// ---------------------------------------------------------------------------

fn handle_list_local_devices() -> Result<serde_json::Value, JsonRpcError> {
    let local_devices = extender_server::device::enumerate_devices().map_err(|e| {
        tracing::debug!("USB enumeration failed: {e}");
        JsonRpcError::internal_error("USB enumeration failed".to_string())
    })?;

    let devices: Vec<DeviceInfo> = local_devices
        .iter()
        .map(|d| local_to_device_info(d, false))
        .collect();

    serde_json::to_value(&devices).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

async fn handle_list_exported_devices(state: &ApiState) -> Result<serde_json::Value, JsonRpcError> {
    let registry = &state.registry;
    let inner = registry.inner().read().await;

    let devices: Vec<ExportedDeviceInfo> = inner
        .iter()
        .map(|(bus_id, exported)| ExportedDeviceInfo {
            bus_id: bus_id.clone(),
            vendor_id: exported.device.vendor_id,
            product_id: exported.device.product_id,
            manufacturer: exported.device.manufacturer.clone(),
            product: exported.device.product.clone(),
            device_class: exported.device.device_class,
            speed: speed_to_api(exported.device.speed),
            num_clients: if exported.active_session.is_some() {
                1
            } else {
                0
            },
        })
        .collect();

    serde_json::to_value(&devices).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

async fn handle_list_remote_devices(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let host = params
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'host'"))?;
    let port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'port'"))? as u16;

    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid address: {e}")))?;

    let remote_devices = extender_client::remote::list_remote_devices(addr)
        .await
        .map_err(|e| {
            tracing::debug!("remote list failed: {e}");
            JsonRpcError::internal_error("remote list failed".to_string())
        })?;

    let devices: Vec<DeviceInfo> = remote_devices
        .iter()
        .map(|d| DeviceInfo {
            bus_id: d.busid.clone(),
            vendor_id: d.id_vendor,
            product_id: d.id_product,
            manufacturer: None,
            product: None,
            device_class: d.device_class,
            speed: speed_to_api(d.speed),
            is_bound: false,
        })
        .collect();

    serde_json::to_value(&devices).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

async fn handle_bind_device(
    params: &Option<serde_json::Value>,
    state: &ApiState,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    // Look up the device's VID:PID and check against the ACL policy.
    let local_devices = extender_server::device::enumerate_devices().map_err(|e| {
        tracing::debug!("USB enumeration failed: {e}");
        JsonRpcError::internal_error("USB enumeration failed".to_string())
    })?;
    let device = local_devices
        .iter()
        .find(|d| d.bus_id == bus_id)
        .ok_or_else(|| JsonRpcError::invalid_params(format!("device '{bus_id}' not found")))?;

    if !device_acl::is_device_allowed(device.vendor_id, device.product_id, &state.security) {
        return Err(JsonRpcError::internal_error(format!(
            "device {:04x}:{:04x} is not allowed by ACL policy",
            device.vendor_id, device.product_id
        )));
    }

    state.registry.bind_device(bus_id).await.map_err(|e| {
        tracing::debug!("bind_device failed: {e}");
        JsonRpcError::internal_error("bind device failed".to_string())
    })?;

    Ok(serde_json::json!({"status": "ok", "bus_id": bus_id}))
}

async fn handle_unbind_device(
    params: &Option<serde_json::Value>,
    state: &ApiState,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    let session = state.registry.unbind_device(bus_id).await.map_err(|e| {
        tracing::debug!("unbind_device failed: {e}");
        JsonRpcError::internal_error("unbind device failed".to_string())
    })?;

    Ok(serde_json::json!({
        "status": "ok",
        "bus_id": bus_id,
        "had_active_session": session.is_some()
    }))
}

async fn handle_attach_device(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let host = params
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'host'"))?;
    let port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'port'"))? as u16;
    let bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .map_err(|e| JsonRpcError::invalid_params(format!("invalid address: {e}")))?;

    let client_engine = extender_client::ClientEngine::new().map_err(|e| {
        tracing::debug!("client init failed: {e}");
        JsonRpcError::internal_error("client initialization failed".to_string())
    })?;
    let attached = client_engine
        .attach_device(addr, bus_id)
        .await
        .map_err(|e| {
            tracing::debug!("attach failed: {e}");
            JsonRpcError::internal_error("attach device failed".to_string())
        })?;

    Ok(serde_json::json!({
        "status": "ok",
        "port": attached.port,
        "bus_id": bus_id,
    }))
}

async fn handle_detach_device(
    params: &Option<serde_json::Value>,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let port = params
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'port'"))? as u32;

    let client_engine = extender_client::ClientEngine::new().map_err(|e| {
        tracing::debug!("client init failed: {e}");
        JsonRpcError::internal_error("client initialization failed".to_string())
    })?;
    client_engine.detach_device(port).await.map_err(|e| {
        tracing::debug!("detach failed: {e}");
        JsonRpcError::internal_error("detach device failed".to_string())
    })?;

    Ok(serde_json::json!({"status": "ok", "port": port}))
}

async fn handle_get_status(state: &ApiState) -> Result<serde_json::Value, JsonRpcError> {
    let inner = state.registry.inner().read().await;
    let exported = inner.len();
    let active = inner
        .values()
        .filter(|e| e.active_session.is_some())
        .count();

    let status = DaemonStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        exported_devices: exported as u32,
        imported_devices: 0, // TODO: get from client engine
        active_connections: active as u32,
    };
    serde_json::to_value(&status).map_err(|e| JsonRpcError::internal_error(e.to_string()))
}

fn handle_get_device_info(
    params: &Option<serde_json::Value>,
    _state: &ApiState,
) -> Result<serde_json::Value, JsonRpcError> {
    let params = params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("missing params"))?;
    let bus_id = params
        .get("bus_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("missing 'bus_id'"))?;

    // Try to find in local devices
    let local_devices = extender_server::device::enumerate_devices().map_err(|e| {
        tracing::debug!("device enumeration failed: {e}");
        JsonRpcError::internal_error("device enumeration failed".to_string())
    })?;

    let local = local_devices.iter().find(|d| d.bus_id == bus_id);

    match local {
        Some(d) => {
            let info = local_to_device_info(d, false);
            serde_json::to_value(&info).map_err(|e| JsonRpcError::internal_error(e.to_string()))
        }
        None => Err(JsonRpcError::invalid_params(format!(
            "device '{bus_id}' not found"
        ))),
    }
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn local_to_device_info(d: &extender_server::device::LocalUsbDevice, is_bound: bool) -> DeviceInfo {
    DeviceInfo {
        bus_id: d.bus_id.clone(),
        vendor_id: d.vendor_id,
        product_id: d.product_id,
        manufacturer: d.manufacturer.clone(),
        product: d.product.clone(),
        device_class: d.device_class,
        speed: speed_to_api(d.speed),
        is_bound,
    }
}

fn speed_to_api(speed: u32) -> UsbSpeed {
    match speed {
        1 => UsbSpeed::Low,
        2 => UsbSpeed::Full,
        3 => UsbSpeed::High,
        5 => UsbSpeed::Super,
        _ => UsbSpeed::Unknown,
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use extender_api::{read_message, write_message, JsonRpcRequest};
    use tokio::net::UnixStream;

    fn test_state() -> Arc<ApiState> {
        Arc::new(ApiState::new(
            Arc::new(ExportRegistry::new()),
            SecurityConfig::default(),
        ))
    }

    #[tokio::test]
    async fn test_api_server_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let socket_str = socket_path.to_string_lossy().to_string();

        let shutdown = CancellationToken::new();
        let state = test_state();

        let server_shutdown = shutdown.clone();
        let server_socket = socket_str.clone();
        let server_state = Arc::clone(&state);
        let server_handle = tokio::spawn(async move {
            run_api_server(&server_socket, server_shutdown, server_state)
                .await
                .unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let stream = UnixStream::connect(&socket_str).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();

        // Send a get_status request.
        let req = JsonRpcRequest::new("get_status", None, 1);
        let req_bytes = serde_json::to_vec(&req).unwrap();
        write_message(&mut writer, &req_bytes).await.unwrap();

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

        shutdown.cancel();
        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_bind_requires_params() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test2.sock");
        let socket_str = socket_path.to_string_lossy().to_string();

        let shutdown = CancellationToken::new();
        let state = test_state();

        let server_shutdown = shutdown.clone();
        let server_socket = socket_str.clone();
        let server_state = Arc::clone(&state);
        tokio::spawn(async move {
            let _ = run_api_server(&server_socket, server_shutdown, server_state).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let stream = UnixStream::connect(&socket_str).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();

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
