//! API client for communicating with the Extender daemon.
//!
//! On Unix, connects via a Unix domain socket. On Windows, connects via TCP to
//! localhost on the port specified by the socket_path string.

#[cfg(unix)]
use std::path::Path;

use extender_api::{
    read_message, write_message, ApiMethod, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
};
#[cfg(windows)]
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;

/// Error type for daemon client operations.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the daemon socket.
    ConnectionFailed(std::io::Error),
    /// I/O or framing error during communication.
    Framing(extender_api::FramingError),
    /// Failed to serialize the request.
    Serialize(serde_json::Error),
    /// Failed to deserialize the response.
    Deserialize(serde_json::Error),
    /// The daemon returned a JSON-RPC error.
    RpcError(JsonRpcError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(e) => {
                write!(f, "cannot connect to daemon: {e} (is the daemon running?)")
            }
            Self::Framing(e) => write!(f, "communication error: {e}"),
            Self::Serialize(e) => write!(f, "failed to serialize request: {e}"),
            Self::Deserialize(e) => write!(f, "failed to deserialize response: {e}"),
            Self::RpcError(e) => {
                write!(f, "daemon error ({}): {}", e.code, e.message)?;
                if let Some(data) = &e.data {
                    write!(f, " - {data}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ClientError {}

/// Connect to the daemon's Unix socket at the given path.
#[cfg(unix)]
pub async fn connect_to_daemon(socket_path: &str) -> Result<UnixStream, ClientError> {
    let path = Path::new(socket_path);
    UnixStream::connect(path)
        .await
        .map_err(ClientError::ConnectionFailed)
}

/// Connect to the daemon via TCP on localhost (Windows).
///
/// `socket_path` is interpreted as a port number (defaulting to 9241).
#[cfg(windows)]
pub async fn connect_to_daemon(socket_path: &str) -> Result<TcpStream, ClientError> {
    let port: u16 = socket_path.parse().unwrap_or(9241);
    let addr = format!("127.0.0.1:{}", port);
    TcpStream::connect(&addr)
        .await
        .map_err(ClientError::ConnectionFailed)
}

/// Send a JSON-RPC request to the daemon and return the parsed response.
///
/// This uses the length-prefixed framing from `extender_api::jsonrpc`.
pub async fn call_daemon(
    socket_path: &str,
    method: ApiMethod,
) -> Result<serde_json::Value, ClientError> {
    let mut stream = connect_to_daemon(socket_path).await?;

    let (method_name, params) = method_to_rpc(&method);
    let request = JsonRpcRequest::new(method_name, params, 1);
    let request_bytes = serde_json::to_vec(&request).map_err(ClientError::Serialize)?;

    let (mut reader, mut writer) = stream.split();

    write_message(&mut writer, &request_bytes)
        .await
        .map_err(ClientError::Framing)?;

    let response_bytes = read_message(&mut reader)
        .await
        .map_err(ClientError::Framing)?;
    let response: JsonRpcResponse =
        serde_json::from_slice(&response_bytes).map_err(ClientError::Deserialize)?;

    if let Some(err) = response.error {
        return Err(ClientError::RpcError(err));
    }

    Ok(response.result.unwrap_or(serde_json::Value::Null))
}

/// Convert an `ApiMethod` into a JSON-RPC method name and optional params value.
fn method_to_rpc(method: &ApiMethod) -> (&'static str, Option<serde_json::Value>) {
    let name = method.method_name();
    let params = match method {
        ApiMethod::ListLocalDevices => None,
        ApiMethod::ListExportedDevices => None,
        ApiMethod::GetStatus => None,
        ApiMethod::ListRemoteDevices { host, port } => {
            Some(serde_json::json!({"host": host, "port": port}))
        }
        ApiMethod::BindDevice { bus_id } => Some(serde_json::json!({"bus_id": bus_id})),
        ApiMethod::UnbindDevice { bus_id } => Some(serde_json::json!({"bus_id": bus_id})),
        ApiMethod::AttachDevice { host, port, bus_id } => {
            Some(serde_json::json!({"host": host, "port": port, "bus_id": bus_id}))
        }
        ApiMethod::DetachDevice { port } => Some(serde_json::json!({"port": port})),
        ApiMethod::GetDeviceInfo { bus_id } => Some(serde_json::json!({"bus_id": bus_id})),
        ApiMethod::Subscribe { events } => Some(serde_json::json!({"events": events})),
    };
    (name, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_method_to_rpc_no_params() {
        let (name, params) = method_to_rpc(&ApiMethod::ListLocalDevices);
        assert_eq!(name, "list_local_devices");
        assert!(params.is_none());

        let (name, params) = method_to_rpc(&ApiMethod::GetStatus);
        assert_eq!(name, "get_status");
        assert!(params.is_none());
    }

    #[test]
    fn test_method_to_rpc_with_params() {
        let (name, params) = method_to_rpc(&ApiMethod::BindDevice {
            bus_id: "1-1".to_string(),
        });
        assert_eq!(name, "bind_device");
        let p = params.unwrap();
        assert_eq!(p["bus_id"], "1-1");
    }

    #[test]
    fn test_method_to_rpc_attach() {
        let (name, params) = method_to_rpc(&ApiMethod::AttachDevice {
            host: "10.0.0.1".to_string(),
            port: 3240,
            bus_id: "2-1".to_string(),
        });
        assert_eq!(name, "attach_device");
        let p = params.unwrap();
        assert_eq!(p["host"], "10.0.0.1");
        assert_eq!(p["port"], 3240);
        assert_eq!(p["bus_id"], "2-1");
    }

    #[test]
    fn test_method_to_rpc_list_remote() {
        let (name, params) = method_to_rpc(&ApiMethod::ListRemoteDevices {
            host: "server.local".to_string(),
            port: 3240,
        });
        assert_eq!(name, "list_remote_devices");
        let p = params.unwrap();
        assert_eq!(p["host"], "server.local");
        assert_eq!(p["port"], 3240);
    }

    #[test]
    fn test_client_error_display() {
        let err = ClientError::RpcError(JsonRpcError::method_not_found("foo"));
        let msg = format!("{err}");
        assert!(msg.contains("daemon error"));
        assert!(msg.contains("-32601"));
    }
}
