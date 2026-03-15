//! Signal handling: SIGHUP (config reload), SIGTERM/SIGINT (graceful shutdown).

use tokio_util::sync::CancellationToken;
use tracing::info;

/// Spawn a background task that listens for OS signals and acts accordingly.
///
/// - `SIGTERM` / `SIGINT` — trigger graceful shutdown via the cancellation token.
/// - `SIGHUP` — invoke the `on_reload` callback (e.g., config reload).
///
/// Returns a `JoinHandle` for the signal listener task.
pub fn spawn_signal_handler(
    shutdown: CancellationToken,
    on_reload: impl Fn() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        signal_loop(shutdown, on_reload).await;
    })
}

#[cfg(unix)]
async fn signal_loop(shutdown: CancellationToken, on_reload: impl Fn() + Send + 'static) {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
    let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM, initiating graceful shutdown");
                shutdown.cancel();
                break;
            }
            _ = sigint.recv() => {
                info!("received SIGINT, initiating graceful shutdown");
                shutdown.cancel();
                break;
            }
            _ = sighup.recv() => {
                info!("received SIGHUP, reloading configuration");
                on_reload();
            }
            _ = shutdown.cancelled() => {
                // Someone else triggered shutdown.
                break;
            }
        }
    }
}

#[cfg(not(unix))]
async fn signal_loop(shutdown: CancellationToken, _on_reload: impl Fn() + Send + 'static) {
    // On non-Unix, we can only handle Ctrl+C.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl+C, initiating graceful shutdown");
            shutdown.cancel();
        }
        _ = shutdown.cancelled() => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cancellation_token_shutdown() {
        let token = CancellationToken::new();
        let child = token.child_token();

        // Simulate external shutdown trigger.
        let token_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        // The signal handler should exit when the token is cancelled.
        let handle = spawn_signal_handler(child.clone(), || {});
        handle.await.unwrap();
        assert!(token.is_cancelled());
    }
}
