//! TCP listener and connection acceptor for the USB/IP server.
//!
//! [`ServerEngine`] binds a TCP listener and spawns a task per inbound
//! connection. Each connection is dispatched to the per-connection handler
//! in [`crate::connection`].
//!
//! When TLS is configured, accepted connections are wrapped with
//! [`tokio_rustls::TlsAcceptor`] before being handed to the connection handler.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::connection::handle_connection;
use crate::error::ServerError;
use crate::export::ExportRegistry;
use crate::tls::TlsServerConfig;

/// The USB/IP server engine.
///
/// Manages the TCP listener, connection accept loop, and the export
/// registry. External code (the daemon) constructs this and calls
/// [`ServerEngine::run`] to start serving.
pub struct ServerEngine {
    listener: TcpListener,
    registry: Arc<ExportRegistry>,
    tls_acceptor: Option<TlsAcceptor>,
}

impl ServerEngine {
    /// Create a new server engine bound to the given address (plain TCP).
    ///
    /// This binds the TCP listener immediately but does not start
    /// accepting connections until [`run`](ServerEngine::run) is called.
    pub async fn new(addr: SocketAddr) -> Result<Self, ServerError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(ServerError::ListenerBind)?;

        let local_addr = listener.local_addr().map_err(ServerError::ListenerBind)?;
        tracing::info!(%local_addr, "server engine bound (plain TCP)");

        Ok(ServerEngine {
            listener,
            registry: Arc::new(ExportRegistry::new()),
            tls_acceptor: None,
        })
    }

    /// Create a new server engine bound to the given address with TLS enabled.
    ///
    /// All accepted connections are wrapped with TLS. Plain TCP connections
    /// will fail the TLS handshake and be rejected.
    pub async fn new_tls(
        addr: SocketAddr,
        tls_config: &TlsServerConfig,
    ) -> Result<Self, ServerError> {
        let acceptor = tls_config.build_acceptor()?;

        let listener = TcpListener::bind(addr)
            .await
            .map_err(ServerError::ListenerBind)?;

        let local_addr = listener.local_addr().map_err(ServerError::ListenerBind)?;
        tracing::info!(%local_addr, "server engine bound (TLS enabled)");

        Ok(ServerEngine {
            listener,
            registry: Arc::new(ExportRegistry::new()),
            tls_acceptor: Some(acceptor),
        })
    }

    /// Create a server engine from an already-bound listener and registry.
    ///
    /// Useful for testing or when the daemon manages the registry externally.
    pub fn from_parts(listener: TcpListener, registry: Arc<ExportRegistry>) -> Self {
        ServerEngine {
            listener,
            registry,
            tls_acceptor: None,
        }
    }

    /// Create a server engine from parts with optional TLS.
    pub fn from_parts_tls(
        listener: TcpListener,
        registry: Arc<ExportRegistry>,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Self {
        ServerEngine {
            listener,
            registry,
            tls_acceptor,
        }
    }

    /// Returns whether TLS is enabled on this server engine.
    pub fn tls_enabled(&self) -> bool {
        self.tls_acceptor.is_some()
    }

    /// Get a reference to the export registry.
    pub fn registry(&self) -> &Arc<ExportRegistry> {
        &self.registry
    }

    /// Get the local address the server is listening on.
    pub fn local_addr(&self) -> Result<SocketAddr, ServerError> {
        self.listener
            .local_addr()
            .map_err(ServerError::ListenerBind)
    }

    /// Run the server accept loop.
    ///
    /// This accepts TCP connections in a loop and spawns a task for each
    /// one. The loop runs until the provided cancellation token signals
    /// shutdown, or indefinitely if no shutdown mechanism is used.
    ///
    /// Each connection is handled by [`crate::connection::handle_connection`].
    pub async fn run(self) -> Result<(), ServerError> {
        let local_addr = self
            .listener
            .local_addr()
            .map_err(ServerError::ListenerBind)?;
        tracing::info!(%local_addr, "server engine accepting connections");

        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!("failed to accept connection: {}", e);
                    continue;
                }
            };

            let registry = Arc::clone(&self.registry);
            let tls_acceptor = self.tls_acceptor.clone();

            tokio::spawn(async move {
                if let Some(acceptor) = tls_acceptor {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            handle_connection(tls_stream, registry, peer).await;
                        }
                        Err(e) => {
                            tracing::warn!(%peer, "TLS handshake failed: {}", e);
                        }
                    }
                } else {
                    handle_connection(stream, registry, peer).await;
                }
            });
        }
    }

    /// Run the server accept loop with a graceful shutdown signal.
    ///
    /// Stops accepting new connections when the shutdown future resolves.
    /// Existing connections continue running until they complete.
    pub async fn run_until_shutdown<F: std::future::Future>(
        self,
        shutdown: F,
    ) -> Result<(), ServerError> {
        let local_addr = self
            .listener
            .local_addr()
            .map_err(ServerError::ListenerBind)?;
        tracing::info!(%local_addr, "server engine accepting connections (with shutdown)");

        tokio::select! {
            result = self.accept_loop() => result,
            _ = shutdown => {
                tracing::info!("shutdown signal received, stopping accept loop");
                Ok(())
            }
        }
    }

    /// Internal accept loop extracted for use with tokio::select.
    async fn accept_loop(&self) -> Result<(), ServerError> {
        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!("failed to accept connection: {}", e);
                    continue;
                }
            };

            let registry = Arc::clone(&self.registry);
            let tls_acceptor = self.tls_acceptor.clone();

            tokio::spawn(async move {
                if let Some(acceptor) = tls_acceptor {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            handle_connection(tls_stream, registry, peer).await;
                        }
                        Err(e) => {
                            tracing::warn!(%peer, "TLS handshake failed: {}", e);
                        }
                    }
                } else {
                    handle_connection(stream, registry, peer).await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_engine_bind_and_local_addr() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let engine = ServerEngine::new(addr).await.unwrap();
        let local = engine.local_addr().unwrap();
        assert!(local.port() > 0);
        assert_eq!(local.ip(), std::net::Ipv4Addr::LOCALHOST);
        assert!(!engine.tls_enabled());
    }

    #[tokio::test]
    async fn test_engine_from_parts() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let registry = Arc::new(ExportRegistry::new());
        let engine = ServerEngine::from_parts(listener, registry);
        assert!(engine.local_addr().is_ok());
        assert!(!engine.tls_enabled());
    }

    #[tokio::test]
    async fn test_engine_registry_access() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let engine = ServerEngine::new(addr).await.unwrap();
        let devices = engine.registry().list_devices().await.unwrap();
        assert!(devices.is_empty());
    }

    #[tokio::test]
    async fn test_engine_shutdown() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let engine = ServerEngine::new(addr).await.unwrap();

        // Shutdown immediately.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tx.send(()).unwrap();

        let result = engine
            .run_until_shutdown(async {
                rx.await.ok();
            })
            .await;
        assert!(result.is_ok());
    }
}
