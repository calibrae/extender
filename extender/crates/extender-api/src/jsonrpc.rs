//! JSON-RPC 2.0 request/response/error framing and length-prefixed message I/O.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Create a new JSON-RPC 2.0 request.
    pub fn new(method: impl Into<String>, params: Option<serde_json::Value>, id: u64) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
            id: Some(serde_json::Value::Number(id.into())),
        }
    }

    /// Create a notification (no id field).
    pub fn notification(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
            id: None,
        }
    }
}

/// JSON-RPC 2.0 successful response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    /// Create a successful response.
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Create an error response.
    pub fn error(id: Option<serde_json::Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(error),
            id,
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// Standard JSON-RPC error codes.
impl JsonRpcError {
    pub fn parse_error(detail: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: "Parse error".to_string(),
            data: Some(serde_json::Value::String(detail.into())),
        }
    }

    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: "Invalid Request".to_string(),
            data: Some(serde_json::Value::String(detail.into())),
        }
    }

    pub fn method_not_found(method: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: "Method not found".to_string(),
            data: Some(serde_json::Value::String(method.into())),
        }
    }

    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: "Invalid params".to_string(),
            data: Some(serde_json::Value::String(detail.into())),
        }
    }

    pub fn internal_error(detail: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: "Internal error".to_string(),
            data: Some(serde_json::Value::String(detail.into())),
        }
    }
}

/// Errors from message framing I/O.
#[derive(Debug, Error)]
pub enum FramingError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("message too large: {0} bytes (max {MAX_MESSAGE_SIZE})")]
    MessageTooLarge(u32),
    #[error("connection closed")]
    ConnectionClosed,
}

/// Maximum message size: 64 KiB.
pub const MAX_MESSAGE_SIZE: u32 = 65536;

/// Write a length-prefixed message: 4-byte big-endian length followed by the payload.
pub async fn write_message<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    msg: &[u8],
) -> Result<(), FramingError> {
    let len = msg.len() as u32;
    if len > MAX_MESSAGE_SIZE {
        return Err(FramingError::MessageTooLarge(len));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(msg).await?;
    writer.flush().await?;
    Ok(())
}

/// Read a length-prefixed message: 4-byte big-endian length followed by the payload.
pub async fn read_message<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, FramingError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(FramingError::ConnectionClosed);
        }
        Err(e) => return Err(FramingError::Io(e)),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_MESSAGE_SIZE {
        return Err(FramingError::MessageTooLarge(len));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new("get_status", None, 1);
        let json = serde_json::to_string(&req).unwrap();
        let parsed: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.jsonrpc, "2.0");
        assert_eq!(parsed.method, "get_status");
        assert_eq!(parsed.id, Some(serde_json::Value::Number(1.into())));
    }

    #[test]
    fn test_notification_has_no_id() {
        let notif = JsonRpcRequest::notification("device_plugged", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn test_success_response() {
        let resp = JsonRpcResponse::success(
            Some(serde_json::Value::Number(1.into())),
            serde_json::json!({"status": "ok"}),
        );
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.result.is_some());
        assert!(parsed.error.is_none());
    }

    #[test]
    fn test_error_response() {
        let resp = JsonRpcResponse::error(
            Some(serde_json::Value::Number(1.into())),
            JsonRpcError::method_not_found("unknown_method"),
        );
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.result.is_none());
        assert!(parsed.error.is_some());
        assert_eq!(parsed.error.unwrap().code, -32601);
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(JsonRpcError::parse_error("bad json").code, -32700);
        assert_eq!(JsonRpcError::invalid_request("missing field").code, -32600);
        assert_eq!(JsonRpcError::method_not_found("foo").code, -32601);
        assert_eq!(JsonRpcError::invalid_params("wrong type").code, -32602);
        assert_eq!(JsonRpcError::internal_error("panic").code, -32603);
    }

    #[tokio::test]
    async fn test_framing_round_trip() {
        let original = b"hello, JSON-RPC!";
        let mut buf = Vec::new();
        write_message(&mut buf, original).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received = read_message(&mut cursor).await.unwrap();
        assert_eq!(received, original);
    }

    #[tokio::test]
    async fn test_framing_empty_message() {
        let original = b"";
        let mut buf = Vec::new();
        write_message(&mut buf, original).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received = read_message(&mut cursor).await.unwrap();
        assert_eq!(received, original.to_vec());
    }

    #[tokio::test]
    async fn test_framing_connection_closed() {
        let buf: Vec<u8> = vec![];
        let mut cursor = std::io::Cursor::new(buf);
        let result = read_message(&mut cursor).await;
        assert!(matches!(result, Err(FramingError::ConnectionClosed)));
    }
}
