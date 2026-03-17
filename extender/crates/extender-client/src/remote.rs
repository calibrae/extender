//! Remote device listing: connect to a USB/IP server and query exported devices.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;

use extender_protocol::codec::{read_op_message, write_op_message};
use extender_protocol::{OpMessage, OpRepDevlist, OpReqDevlist};

use crate::error::ClientError;
use crate::tls::TlsClientConfig;
use crate::types::RemoteDevice;

/// Default connect timeout in seconds.
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// Query a remote USB/IP server for its list of exported devices (plain TCP).
///
/// Opens a TCP connection to `addr`, sends OP_REQ_DEVLIST, parses
/// OP_REP_DEVLIST, and returns a list of user-friendly `RemoteDevice` structs.
pub async fn list_remote_devices(addr: SocketAddr) -> Result<Vec<RemoteDevice>, ClientError> {
    let connect_timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

    // Connect with timeout
    let stream = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ClientError::ConnectTimeout {
            addr,
            timeout_secs: CONNECT_TIMEOUT_SECS,
        })?
        .map_err(ClientError::Io)?;

    let (mut reader, mut writer) = stream.into_split();
    devlist_exchange(&mut reader, &mut writer).await
}

/// Query a remote USB/IP server for its list of exported devices (TLS).
///
/// Same as [`list_remote_devices`] but wraps the TCP connection with TLS.
pub async fn list_remote_devices_tls(
    addr: SocketAddr,
    tls_config: &TlsClientConfig,
) -> Result<Vec<RemoteDevice>, ClientError> {
    let connect_timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);

    let tcp_stream = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ClientError::ConnectTimeout {
            addr,
            timeout_secs: CONNECT_TIMEOUT_SECS,
        })?
        .map_err(ClientError::Io)?;

    let connector = tls_config.build_connector()?;
    let server_name = tls_config.server_name()?;

    let tls_stream = connector
        .connect(server_name, tcp_stream)
        .await
        .map_err(|e| ClientError::Tls(format!("TLS handshake failed: {e}")))?;

    let (reader, writer) = tokio::io::split(tls_stream);
    let mut reader = reader;
    let mut writer = writer;
    devlist_exchange(&mut reader, &mut writer).await
}

/// Perform the DEVLIST protocol exchange over any async stream.
async fn devlist_exchange<R, W>(
    reader: &mut R,
    writer: &mut W,
) -> Result<Vec<RemoteDevice>, ClientError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Send OP_REQ_DEVLIST
    let req = OpMessage::ReqDevlist(OpReqDevlist);
    write_op_message(writer, &req).await?;

    // Read OP_REP_DEVLIST
    let reply = read_op_message(reader).await?;

    match reply {
        OpMessage::RepDevlist(OpRepDevlist { status, devices }) => {
            if status != 0 {
                return Err(ClientError::DevlistError { status });
            }
            let remote_devices = devices.iter().map(RemoteDevice::from).collect();
            Ok(remote_devices)
        }
        other => Err(ClientError::Protocol(
            extender_protocol::ProtocolError::InvalidOpCode(match other {
                OpMessage::ReqDevlist(_) => 0x8005,
                OpMessage::RepDevlist(_) => 0x0005,
                OpMessage::ReqImport(_) => 0x8003,
                OpMessage::RepImport(_) => 0x0003,
            }),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use extender_protocol::{OpRepDevlist, UsbDevice, UsbInterface};
    use tokio::net::TcpListener;

    /// Create a mock server that responds with a canned DEVLIST reply.
    async fn mock_devlist_server(reply: OpRepDevlist) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // Read the request
            let msg = read_op_message(&mut reader).await.unwrap();
            assert!(matches!(msg, OpMessage::ReqDevlist(_)));

            // Send the reply
            let reply_msg = OpMessage::RepDevlist(reply);
            write_op_message(&mut writer, &reply_msg).await.unwrap();
        });

        addr
    }

    #[tokio::test]
    async fn test_list_remote_devices_empty() {
        let addr = mock_devlist_server(OpRepDevlist {
            status: 0,
            devices: vec![],
        })
        .await;

        let devices = list_remote_devices(addr).await.unwrap();
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn test_list_remote_devices_one_device() {
        let dev = UsbDevice {
            path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
            busid: UsbDevice::busid_from_str("1-1").unwrap(),
            busnum: 1,
            devnum: 2,
            speed: 3,
            id_vendor: 0x1234,
            id_product: 0x5678,
            bcd_device: 0x0100,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 1,
            interfaces: vec![UsbInterface {
                interface_class: 0x03,
                interface_subclass: 0x01,
                interface_protocol: 0x02,
                padding: 0,
            }],
        };

        let addr = mock_devlist_server(OpRepDevlist {
            status: 0,
            devices: vec![dev],
        })
        .await;

        let devices = list_remote_devices(addr).await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].busid, "1-1");
        assert_eq!(devices[0].id_vendor, 0x1234);
        assert_eq!(devices[0].id_product, 0x5678);
        assert_eq!(devices[0].speed, 3);
        assert_eq!(devices[0].interface_classes, vec![0x03]);
    }

    #[tokio::test]
    async fn test_list_remote_devices_error_status() {
        let addr = mock_devlist_server(OpRepDevlist {
            status: 1,
            devices: vec![],
        })
        .await;

        let result = list_remote_devices(addr).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ClientError::DevlistError { status: 1 }
        ));
    }

    #[tokio::test]
    async fn test_list_remote_devices_connect_timeout() {
        // Use a non-routable address to trigger timeout
        let addr: SocketAddr = "192.0.2.1:3240".parse().unwrap();
        let result = list_remote_devices(addr).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ClientError::ConnectTimeout { timeout_secs, .. } => {
                assert_eq!(timeout_secs, CONNECT_TIMEOUT_SECS);
            }
            other => panic!("expected ConnectTimeout, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_list_remote_devices_multiple() {
        let dev1 = UsbDevice {
            path: UsbDevice::path_from_str("/sys/devices/usb1/1-1"),
            busid: UsbDevice::busid_from_str("1-1").unwrap(),
            busnum: 1,
            devnum: 2,
            speed: 2,
            id_vendor: 0x0001,
            id_product: 0x0002,
            bcd_device: 0x0100,
            device_class: 0x09,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 0,
            interfaces: vec![],
        };
        let dev2 = UsbDevice {
            path: UsbDevice::path_from_str("/sys/devices/usb2/2-3"),
            busid: UsbDevice::busid_from_str("2-3").unwrap(),
            busnum: 2,
            devnum: 3,
            speed: 5,
            id_vendor: 0xAAAA,
            id_product: 0xBBBB,
            bcd_device: 0x0200,
            device_class: 0x08,
            device_subclass: 0x06,
            device_protocol: 0x50,
            configuration_value: 1,
            num_configurations: 1,
            num_interfaces: 1,
            interfaces: vec![UsbInterface {
                interface_class: 0x08,
                interface_subclass: 0x06,
                interface_protocol: 0x50,
                padding: 0,
            }],
        };

        let addr = mock_devlist_server(OpRepDevlist {
            status: 0,
            devices: vec![dev1, dev2],
        })
        .await;

        let devices = list_remote_devices(addr).await.unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].busid, "1-1");
        assert_eq!(devices[0].speed, 2);
        assert_eq!(devices[1].busid, "2-3");
        assert_eq!(devices[1].id_vendor, 0xAAAA);
    }
}
