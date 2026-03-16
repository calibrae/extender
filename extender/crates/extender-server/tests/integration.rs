//! Integration tests for the server engine, export registry, and protocol exchange.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use extender_protocol::codec::{read_op_message, write_op_message};
use extender_protocol::{OpMessage, OpReqDevlist, OpReqImport, UsbDevice};

use extender_server::engine::ServerEngine;
use extender_server::export::ExportRegistry;

/// Helper: start a server engine on a random port and return its address.
async fn start_test_server() -> (SocketAddr, Arc<ExportRegistry>) {
    let registry = Arc::new(ExportRegistry::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = ServerEngine::from_parts(listener, Arc::clone(&registry));

    tokio::spawn(async move {
        engine.run().await.ok();
    });

    (addr, registry)
}

// ── DEVLIST Integration Tests ────────────────────────────────────────

#[tokio::test]
async fn test_devlist_empty_registry() {
    let (addr, _registry) = start_test_server().await;

    let mut stream = TcpStream::connect(addr).await.unwrap();

    // Send OP_REQ_DEVLIST.
    let req = OpMessage::ReqDevlist(OpReqDevlist);
    write_op_message(&mut stream, &req).await.unwrap();

    // Read OP_REP_DEVLIST.
    let reply = read_op_message(&mut stream).await.unwrap();
    match reply {
        OpMessage::RepDevlist(rep) => {
            assert_eq!(rep.status, 0);
            assert!(rep.devices.is_empty());
        }
        other => panic!("expected RepDevlist, got: {:?}", other),
    }

    // Server should close the connection after DEVLIST.
    let mut buf = [0u8; 1];
    let n = stream.read(&mut buf).await.unwrap();
    assert_eq!(n, 0, "expected connection to close after DEVLIST");
}

// ── IMPORT Integration Tests ─────────────────────────────────────────

#[tokio::test]
async fn test_import_device_not_bound() {
    let (addr, _registry) = start_test_server().await;

    let mut stream = TcpStream::connect(addr).await.unwrap();

    // Send OP_REQ_IMPORT for a device that is not bound.
    let busid = UsbDevice::busid_from_str("1-99").unwrap();
    let req = OpMessage::ReqImport(OpReqImport { busid });
    write_op_message(&mut stream, &req).await.unwrap();

    // Read OP_REP_IMPORT.
    let reply = read_op_message(&mut stream).await.unwrap();
    match reply {
        OpMessage::RepImport(rep) => {
            assert_ne!(rep.status, 0, "expected non-zero status for unbound device");
            assert!(rep.device.is_none());
        }
        other => panic!("expected RepImport, got: {:?}", other),
    }
}

// ── ExportRegistry Unit Tests (more thorough) ────────────────────────

#[tokio::test]
async fn test_registry_unbind_not_bound() {
    let registry = ExportRegistry::new();
    let result = registry.unbind_device("1-1").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_registry_acquire_not_bound() {
    let registry = ExportRegistry::new();
    let result = registry.try_acquire("1-1").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_registry_list_empty() {
    let registry = ExportRegistry::new();
    let devices = registry.list_devices().await.unwrap();
    assert!(devices.is_empty());
}

// ── Protocol Exchange with Multiple Connections ──────────────────────

#[tokio::test]
async fn test_multiple_devlist_connections() {
    let (addr, _registry) = start_test_server().await;

    // Multiple clients can query DEVLIST concurrently.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let connect_addr = addr;
        handles.push(tokio::spawn(async move {
            let addr = connect_addr;
            let mut stream = TcpStream::connect(addr).await.unwrap();
            let req = OpMessage::ReqDevlist(OpReqDevlist);
            write_op_message(&mut stream, &req).await.unwrap();
            let reply = read_op_message(&mut stream).await.unwrap();
            match reply {
                OpMessage::RepDevlist(rep) => {
                    assert_eq!(rep.status, 0);
                }
                other => panic!("expected RepDevlist, got: {:?}", other),
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn test_import_then_devlist_separate_connections() {
    let (addr, _registry) = start_test_server().await;

    // First connection: try to import a non-existent device.
    {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let busid = UsbDevice::busid_from_str("1-1").unwrap();
        let req = OpMessage::ReqImport(OpReqImport { busid });
        write_op_message(&mut stream, &req).await.unwrap();
        let reply = read_op_message(&mut stream).await.unwrap();
        match reply {
            OpMessage::RepImport(rep) => {
                assert_ne!(rep.status, 0);
            }
            _ => panic!("expected RepImport"),
        }
    }

    // Second connection: DEVLIST should still work fine.
    {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = OpMessage::ReqDevlist(OpReqDevlist);
        write_op_message(&mut stream, &req).await.unwrap();
        let reply = read_op_message(&mut stream).await.unwrap();
        match reply {
            OpMessage::RepDevlist(rep) => {
                assert_eq!(rep.status, 0);
            }
            _ => panic!("expected RepDevlist"),
        }
    }
}
