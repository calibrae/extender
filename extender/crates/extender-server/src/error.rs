//! Server error types.

use thiserror::Error;

/// Errors that can occur in the server USB access layer.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Failed to initialize the USB context (libusb).
    #[error("failed to initialize USB context: {0}")]
    UsbContextInit(#[source] rusb::Error),

    /// Failed to enumerate USB devices.
    #[error("failed to enumerate USB devices: {0}")]
    Enumeration(#[source] rusb::Error),

    /// The requested device was not found.
    #[error("device not found: {bus_id}")]
    DeviceNotFound { bus_id: String },

    /// The device is already in use by another process.
    #[error("device {bus_id} is in use by another process")]
    DeviceInUse { bus_id: String },

    /// Failed to open the device.
    #[error("failed to open device {bus_id}: {source}")]
    OpenDevice {
        bus_id: String,
        #[source]
        source: rusb::Error,
    },

    /// Failed to set auto-detach kernel driver.
    #[error("failed to set auto-detach kernel driver on {bus_id}: {source}")]
    AutoDetach {
        bus_id: String,
        #[source]
        source: rusb::Error,
    },

    /// Failed to claim an interface.
    #[error("failed to claim interface {interface} on {bus_id}: {source}")]
    ClaimInterface {
        bus_id: String,
        interface: u8,
        #[source]
        source: rusb::Error,
    },

    /// Failed to release an interface.
    #[error("failed to release interface {interface} on {bus_id}: {source}")]
    ReleaseInterface {
        bus_id: String,
        interface: u8,
        #[source]
        source: rusb::Error,
    },

    /// A USB transfer failed.
    #[error("USB transfer error: {0}")]
    Transfer(#[source] rusb::Error),

    /// The transfer timed out.
    #[error("USB transfer timed out")]
    Timeout,

    /// Failed to read a device descriptor.
    #[error("failed to read device descriptor: {0}")]
    Descriptor(#[source] rusb::Error),

    /// Failed to read a configuration descriptor.
    #[error("failed to read configuration descriptor: {0}")]
    ConfigDescriptor(#[source] rusb::Error),

    /// A protocol-level error (e.g., invalid bus ID format).
    #[error("protocol error: {0}")]
    Protocol(#[from] extender_protocol::ProtocolError),

    /// The device is already bound (exported) in the registry.
    #[error("device {bus_id} is already bound for export")]
    DeviceAlreadyBound { bus_id: String },

    /// The device is not bound (not in the export registry).
    #[error("device {bus_id} is not bound for export")]
    DeviceNotBound { bus_id: String },

    /// Failed to bind the TCP listener.
    #[error("failed to bind TCP listener: {0}")]
    ListenerBind(#[source] std::io::Error),

    /// An I/O error occurred during connection handling.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Map a `rusb::Error` to a Linux errno value for USB/IP protocol compatibility.
///
/// The USB/IP protocol uses negative Linux errno values as URB status codes.
/// This function maps libusb/rusb error variants to the corresponding errno.
pub fn rusb_error_to_errno(err: &rusb::Error) -> i32 {
    match err {
        // -EPIPE (broken pipe / stall)
        rusb::Error::Pipe => -32,
        // -ENODEV (no such device / disconnected)
        rusb::Error::NoDevice => -19,
        // -ETIMEDOUT
        rusb::Error::Timeout => -110,
        // -EBUSY
        rusb::Error::Busy => -16,
        // -EOVERFLOW
        rusb::Error::Overflow => -75,
        // -ENOENT (no such file or directory / not found)
        rusb::Error::NotFound => -2,
        // -EACCES (permission denied)
        rusb::Error::Access => -13,
        // -EIO (I/O error) for everything else
        rusb::Error::Io => -5,
        rusb::Error::InvalidParam => -22, // -EINVAL
        rusb::Error::NotSupported => -95, // -EOPNOTSUPP
        rusb::Error::BadDescriptor => -5, // -EIO
        rusb::Error::Interrupted => -4,   // -EINTR
        rusb::Error::NoMem => -12,        // -ENOMEM
        _ => -5,                          // -EIO as fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rusb_error_to_errno_pipe() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Pipe), -32);
    }

    #[test]
    fn test_rusb_error_to_errno_nodev() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::NoDevice), -19);
    }

    #[test]
    fn test_rusb_error_to_errno_timeout() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Timeout), -110);
    }

    #[test]
    fn test_rusb_error_to_errno_busy() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Busy), -16);
    }

    #[test]
    fn test_rusb_error_to_errno_overflow() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Overflow), -75);
    }

    #[test]
    fn test_rusb_error_to_errno_access() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Access), -13);
    }

    #[test]
    fn test_rusb_error_to_errno_io() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Io), -5);
    }

    #[test]
    fn test_rusb_error_to_errno_invalid_param() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::InvalidParam), -22);
    }

    #[test]
    fn test_rusb_error_to_errno_not_supported() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::NotSupported), -95);
    }

    #[test]
    fn test_rusb_error_to_errno_not_found() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::NotFound), -2);
    }

    #[test]
    fn test_rusb_error_to_errno_no_mem() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::NoMem), -12);
    }

    #[test]
    fn test_rusb_error_to_errno_interrupted() {
        assert_eq!(rusb_error_to_errno(&rusb::Error::Interrupted), -4);
    }
}
