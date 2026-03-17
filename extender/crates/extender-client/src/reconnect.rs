//! Auto-reconnection with exponential backoff for USB/IP device imports.
//!
//! When a connection drops, the [`attach_with_reconnect`] function retries
//! the import with configurable exponential backoff and jitter, allowing
//! the server-side grace period to keep the device reserved.

use std::net::SocketAddr;
use std::time::Duration;

use crate::engine::ClientEngine;
use crate::error::ClientError;
use crate::types::AttachedDevice;

/// Policy controlling automatic reconnection behaviour.
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Whether auto-reconnect is enabled.
    pub enabled: bool,
    /// Maximum number of retries. 0 means unlimited.
    pub max_retries: u32,
    /// Initial delay between retries.
    pub initial_delay: Duration,
    /// Maximum delay between retries (caps the exponential growth).
    pub max_delay: Duration,
    /// Multiplicative factor applied to the delay after each retry.
    pub backoff_factor: f64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 0,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_factor: 2.0,
        }
    }
}

impl ReconnectPolicy {
    /// Create a policy with reconnection disabled.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    /// Create a policy from configuration values.
    pub fn from_config(
        enabled: bool,
        max_retries: u32,
        initial_delay_secs: u64,
        max_delay_secs: u64,
    ) -> Self {
        Self {
            enabled,
            max_retries,
            initial_delay: Duration::from_secs(initial_delay_secs),
            max_delay: Duration::from_secs(max_delay_secs),
            backoff_factor: 2.0,
        }
    }

    /// Calculate the delay for a given attempt number (0-indexed).
    ///
    /// The delay grows exponentially: `initial_delay * backoff_factor^attempt`,
    /// capped at `max_delay`. A small amount of jitter (up to 10%) is added
    /// to prevent thundering-herd effects when multiple clients reconnect
    /// simultaneously.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.initial_delay.as_secs_f64() * self.backoff_factor.powi(attempt as i32);
        let capped = base.min(self.max_delay.as_secs_f64());

        // Add deterministic jitter based on the attempt number to avoid
        // all clients retrying at exactly the same time. We use a simple
        // hash-like approach rather than pulling in a random number crate.
        let jitter_fraction = jitter_for_attempt(attempt);
        let jittered = capped * (1.0 + jitter_fraction * 0.1);
        let final_secs = jittered.min(self.max_delay.as_secs_f64());

        Duration::from_secs_f64(final_secs)
    }

    /// Whether another retry should be attempted.
    pub fn should_retry(&self, attempt: u32) -> bool {
        self.enabled && (self.max_retries == 0 || attempt < self.max_retries)
    }
}

/// Simple deterministic jitter in [0.0, 1.0) based on the attempt number.
/// Not cryptographically random, but sufficient to spread out retries.
fn jitter_for_attempt(attempt: u32) -> f64 {
    // Use a simple hash: multiply by a large prime and take fractional part.
    let hash = (attempt as u64).wrapping_mul(2654435761);
    (hash % 1000) as f64 / 1000.0
}

/// Attach a device with automatic reconnection on failure.
///
/// Performs the initial attach, and if it fails (or if the session drops),
/// retries according to the given [`ReconnectPolicy`]. On each retry the
/// function logs the attempt number and delay.
///
/// Returns the attached device on success, or the last error if all retries
/// are exhausted.
pub async fn attach_with_reconnect(
    engine: &ClientEngine,
    addr: SocketAddr,
    busid: &str,
    policy: &ReconnectPolicy,
) -> Result<AttachedDevice, ClientError> {
    if !policy.enabled {
        return engine.attach_device(addr, busid).await;
    }

    let mut attempt: u32 = 0;

    loop {
        match engine.attach_device(addr, busid).await {
            Ok(device) => {
                if attempt > 0 {
                    tracing::info!(
                        busid,
                        %addr,
                        attempt,
                        "reconnected successfully"
                    );
                }
                return Ok(device);
            }
            Err(e) => {
                if !policy.should_retry(attempt) {
                    tracing::error!(
                        busid,
                        %addr,
                        attempt,
                        "all reconnection attempts exhausted: {}",
                        e
                    );
                    return Err(e);
                }

                let delay = policy.delay_for_attempt(attempt);
                tracing::warn!(
                    busid,
                    %addr,
                    attempt = attempt + 1,
                    max_retries = policy.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    "attach failed ({}), retrying in {:?}",
                    e,
                    delay
                );

                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = ReconnectPolicy::default();
        assert!(policy.enabled);
        assert_eq!(policy.max_retries, 0);
        assert_eq!(policy.initial_delay, Duration::from_secs(1));
        assert_eq!(policy.max_delay, Duration::from_secs(30));
        assert_eq!(policy.backoff_factor, 2.0);
    }

    #[test]
    fn test_disabled_policy() {
        let policy = ReconnectPolicy::disabled();
        assert!(!policy.enabled);
        assert!(!policy.should_retry(0));
    }

    #[test]
    fn test_from_config() {
        let policy = ReconnectPolicy::from_config(true, 5, 2, 60);
        assert!(policy.enabled);
        assert_eq!(policy.max_retries, 5);
        assert_eq!(policy.initial_delay, Duration::from_secs(2));
        assert_eq!(policy.max_delay, Duration::from_secs(60));
    }

    #[test]
    fn test_exponential_backoff_calculation() {
        let policy = ReconnectPolicy {
            enabled: true,
            max_retries: 0,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_factor: 2.0,
        };

        // Attempt 0: ~1s (with up to 10% jitter)
        let d0 = policy.delay_for_attempt(0);
        assert!(d0.as_secs_f64() >= 1.0);
        assert!(d0.as_secs_f64() <= 1.1);

        // Attempt 1: ~2s
        let d1 = policy.delay_for_attempt(1);
        assert!(d1.as_secs_f64() >= 2.0);
        assert!(d1.as_secs_f64() <= 2.2);

        // Attempt 2: ~4s
        let d2 = policy.delay_for_attempt(2);
        assert!(d2.as_secs_f64() >= 4.0);
        assert!(d2.as_secs_f64() <= 4.4);

        // Attempt 3: ~8s
        let d3 = policy.delay_for_attempt(3);
        assert!(d3.as_secs_f64() >= 8.0);
        assert!(d3.as_secs_f64() <= 8.8);

        // Attempt 5: should be capped at ~30s (16 * 2 = 32 > 30)
        let d5 = policy.delay_for_attempt(5);
        assert!(d5.as_secs_f64() <= 30.0);
    }

    #[test]
    fn test_backoff_caps_at_max_delay() {
        let policy = ReconnectPolicy {
            enabled: true,
            max_retries: 0,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
            backoff_factor: 2.0,
        };

        // Attempt 10: 1 * 2^10 = 1024, should be capped at 10.
        let d = policy.delay_for_attempt(10);
        assert!(d.as_secs_f64() <= 10.0);
    }

    #[test]
    fn test_should_retry_unlimited() {
        let policy = ReconnectPolicy {
            enabled: true,
            max_retries: 0,
            ..Default::default()
        };
        assert!(policy.should_retry(0));
        assert!(policy.should_retry(100));
        assert!(policy.should_retry(u32::MAX - 1));
    }

    #[test]
    fn test_should_retry_limited() {
        let policy = ReconnectPolicy {
            enabled: true,
            max_retries: 3,
            ..Default::default()
        };
        assert!(policy.should_retry(0));
        assert!(policy.should_retry(1));
        assert!(policy.should_retry(2));
        assert!(!policy.should_retry(3));
        assert!(!policy.should_retry(4));
    }

    #[test]
    fn test_should_retry_disabled() {
        let policy = ReconnectPolicy {
            enabled: false,
            max_retries: 10,
            ..Default::default()
        };
        assert!(!policy.should_retry(0));
    }

    #[test]
    fn test_jitter_deterministic() {
        // Same attempt should always produce same jitter.
        let j1 = jitter_for_attempt(5);
        let j2 = jitter_for_attempt(5);
        assert_eq!(j1, j2);
    }

    #[test]
    fn test_jitter_range() {
        for attempt in 0..100 {
            let j = jitter_for_attempt(attempt);
            assert!(j >= 0.0);
            assert!(j < 1.0);
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn test_attach_with_reconnect_disabled_policy() {
        let engine = ClientEngine::new().unwrap();
        let addr: SocketAddr = "127.0.0.1:3240".parse().unwrap();
        let policy = ReconnectPolicy::disabled();

        // On non-Linux, this returns PlatformNotSupported immediately.
        let result = attach_with_reconnect(&engine, addr, "1-1", &policy).await;
        assert!(matches!(result, Err(ClientError::PlatformNotSupported)));
    }
}
