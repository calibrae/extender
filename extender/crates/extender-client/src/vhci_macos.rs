//! macOS VHCI driver interface via IOKit/DriverKit.
//!
//! On macOS, the ExtenderDriver DriverKit extension exposes a UserClient
//! interface for creating and managing virtual USB devices. This module
//! communicates with the driver via IOKit `IOConnectCall*Method` functions.
//!
//! This module is only compiled on macOS targets.

#![cfg(target_os = "macos")]

use std::ffi::{c_char, c_void, CStr};
use std::ptr;

use crate::error::ClientError;

// ---------------------------------------------------------------------------
// IOKit FFI
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
type io_service_t = u32;
#[allow(non_camel_case_types)]
type io_connect_t = u32;
#[allow(non_camel_case_types)]
type io_object_t = u32;
#[allow(non_camel_case_types)]
type mach_port_t = u32;
#[allow(non_camel_case_types)]
type kern_return_t = i32;

const KERN_SUCCESS: kern_return_t = 0;
#[allow(non_upper_case_globals)]
const kIOMainPortDefault: u32 = 0;

// Minimal CoreFoundation FFI for IOServiceMatching.
type CFMutableDictionaryRef = *mut c_void;
type CFDictionaryRef = *const c_void;

#[link(name = "IOKit", kind = "framework")]
#[allow(dead_code)]
extern "C" {
    fn IOServiceGetMatchingService(
        mainPort: u32,
        matching: CFDictionaryRef,
    ) -> io_service_t;
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceOpen(
        service: io_service_t,
        owningTask: mach_port_t,
        type_: u32,
        connect: *mut io_connect_t,
    ) -> kern_return_t;
    fn IOServiceClose(connect: io_connect_t) -> kern_return_t;
    fn IOConnectCallScalarMethod(
        connect: io_connect_t,
        selector: u32,
        input: *const u64,
        inputCnt: u32,
        output: *mut u64,
        outputCnt: *mut u32,
    ) -> kern_return_t;
    fn IOConnectCallStructMethod(
        connect: io_connect_t,
        selector: u32,
        inputStruct: *const c_void,
        inputStructCnt: usize,
        outputStruct: *mut c_void,
        outputStructCnt: *mut usize,
    ) -> kern_return_t;
    fn IOConnectCallMethod(
        connect: io_connect_t,
        selector: u32,
        input: *const u64,
        inputCnt: u32,
        inputStruct: *const c_void,
        inputStructCnt: usize,
        output: *mut u64,
        outputCnt: *mut u32,
        outputStruct: *mut c_void,
        outputStructCnt: *mut usize,
    ) -> kern_return_t;
    fn IOObjectRelease(object: io_object_t) -> kern_return_t;
}

extern "C" {
    fn mach_task_self() -> mach_port_t;
}

// ---------------------------------------------------------------------------
// ExternalMethod selectors
// ---------------------------------------------------------------------------

/// Create a new virtual device of a given type.
const SELECTOR_CREATE_DEVICE: u32 = 0;
/// Destroy a virtual device by ID.
const SELECTOR_DESTROY_DEVICE: u32 = 1;
/// Submit input data (network → driver) for a virtual device.
const SELECTOR_SUBMIT_INPUT: u32 = 2;
/// Get pending output (driver → network) for a virtual device.
const SELECTOR_GET_PENDING_OUTPUT: u32 = 3;
/// Get the count of active virtual devices.
const SELECTOR_GET_DEVICE_COUNT: u32 = 4;
/// Configure a virtual device with descriptors or parameters.
const SELECTOR_CONFIGURE_DEVICE: u32 = 5;
/// Async completion (daemon → driver) for a request previously surfaced via GET_PENDING_OUTPUT.
const SELECTOR_COMPLETE_REQUEST: u32 = 10;

/// Layout of the ExtenderPendingOutputHeader emitted by the driver. Keep in
/// sync with extender-macos/ExtenderDriver/ExtenderProtocol.h.
const PENDING_OUTPUT_HEADER_SIZE: usize = 24;

// ---------------------------------------------------------------------------
// Device types
// ---------------------------------------------------------------------------

/// HID device (keyboards, mice, gamepads).
pub const DEVICE_TYPE_HID: u32 = 1;
/// Mass storage device (flash drives, external disks).
pub const DEVICE_TYPE_STORAGE: u32 = 2;
/// CDC/ACM serial device.
pub const DEVICE_TYPE_SERIAL: u32 = 3;
/// CDC-ECM/NCM network device.
pub const DEVICE_TYPE_NETWORK: u32 = 4;
/// USB Audio Class device.
pub const DEVICE_TYPE_AUDIO: u32 = 5;

// ---------------------------------------------------------------------------
// Config types for SELECTOR_CONFIGURE_DEVICE
// ---------------------------------------------------------------------------

/// HID report descriptor data.
pub const CONFIG_HID_REPORT_DESCRIPTOR: u32 = 1;
/// Storage geometry (sector size, sector count).
pub const CONFIG_STORAGE_GEOMETRY: u32 = 2;
/// Serial line state (baud rate, data bits, etc.).
pub const CONFIG_SERIAL_LINE_STATE: u32 = 3;
/// Network MAC address.
pub const CONFIG_NETWORK_MAC: u32 = 4;
/// Audio format descriptor.
pub const CONFIG_AUDIO_FORMAT: u32 = 5;
/// Raw USB device descriptor.
pub const CONFIG_DEVICE_DESCRIPTOR: u32 = 6;

// ---------------------------------------------------------------------------
// IOUserClass name matching the DriverKit Info.plist
// ---------------------------------------------------------------------------

/// The IOUserClass name registered by the ExtenderDriver DriverKit extension.
const DRIVER_CLASS_NAME: &CStr = c"ExtenderDriver";

// ---------------------------------------------------------------------------
// MacOSVhciDriver
// ---------------------------------------------------------------------------

/// macOS DriverKit VHCI driver interface.
///
/// Opens an IOKit UserClient connection to the ExtenderDriver DriverKit
/// extension and provides methods to create, configure, and manage virtual
/// USB devices.
pub struct MacOSVhciDriver {
    connect: io_connect_t,
}

impl MacOSVhciDriver {
    /// Open a connection to the ExtenderDriver DriverKit extension.
    ///
    /// Returns an error if the driver is not loaded or the service cannot
    /// be opened.
    pub fn new() -> Result<Self, ClientError> {
        unsafe {
            let matching = IOServiceMatching(DRIVER_CLASS_NAME.as_ptr());
            if matching.is_null() {
                return Err(ClientError::IOKit {
                    code: -1,
                    message: "IOServiceMatching returned null".to_owned(),
                });
            }

            // IOServiceGetMatchingService consumes the matching dictionary.
            let service =
                IOServiceGetMatchingService(kIOMainPortDefault, matching as CFDictionaryRef);
            if service == 0 {
                return Err(ClientError::VhciNotAvailable {
                    reason: "ExtenderDriver service not found (is the DriverKit extension loaded?)"
                        .to_owned(),
                });
            }

            let mut connect: io_connect_t = 0;
            let kr = IOServiceOpen(service, mach_task_self(), 0, &mut connect);
            IOObjectRelease(service);

            if kr != KERN_SUCCESS {
                return Err(ClientError::IOKit {
                    code: kr,
                    message: "IOServiceOpen failed".to_owned(),
                });
            }

            Ok(MacOSVhciDriver { connect })
        }
    }

    /// Create a virtual device of the given type.
    ///
    /// # Arguments
    /// * `device_type` - One of the `DEVICE_TYPE_*` constants.
    /// * `device_id` - A caller-assigned identifier for this device.
    pub fn create_device(&self, device_type: u32, device_id: u32) -> Result<(), ClientError> {
        let input = [device_type as u64, device_id as u64];
        let kr = unsafe {
            IOConnectCallScalarMethod(
                self.connect,
                SELECTOR_CREATE_DEVICE,
                input.as_ptr(),
                input.len() as u32,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        check_kr(kr, "create_device")
    }

    /// Destroy a virtual device.
    pub fn destroy_device(&self, device_id: u32) -> Result<(), ClientError> {
        let input = [device_id as u64];
        let kr = unsafe {
            IOConnectCallScalarMethod(
                self.connect,
                SELECTOR_DESTROY_DEVICE,
                input.as_ptr(),
                input.len() as u32,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        check_kr(kr, "destroy_device")
    }

    /// Send input data to a virtual device (data from network -> driver).
    ///
    /// # Arguments
    /// * `device_id` - The target device ID.
    /// * `endpoint` - The USB endpoint number.
    /// * `data` - The payload bytes.
    pub fn submit_input(
        &self,
        device_id: u32,
        endpoint: u32,
        data: &[u8],
    ) -> Result<(), ClientError> {
        // Dext expects 3 scalars: deviceId, endpoint, dataLength.
        let scalars = [device_id as u64, endpoint as u64, data.len() as u64];
        let kr = unsafe {
            IOConnectCallMethod(
                self.connect,
                SELECTOR_SUBMIT_INPUT,
                scalars.as_ptr(),
                scalars.len() as u32,
                data.as_ptr() as *const c_void,
                data.len(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        check_kr(kr, "submit_input")
    }

    /// Async completion: deliver the response payload for a request that was
    /// previously surfaced via `get_pending_output`. The driver matches by
    /// `request_id` and fires the upstream completion (e.g., HID's CompleteReport
    /// or storage's CompleteIO).
    pub fn complete_request(
        &self,
        device_id: u32,
        request_id: u32,
        status: i32,
        data: &[u8],
    ) -> Result<(), ClientError> {
        let scalars = [device_id as u64, request_id as u64, status as u32 as u64];
        let kr = unsafe {
            IOConnectCallMethod(
                self.connect,
                SELECTOR_COMPLETE_REQUEST,
                scalars.as_ptr(),
                scalars.len() as u32,
                data.as_ptr() as *const c_void,
                data.len(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        check_kr(kr, "complete_request")
    }

    /// Poll for a pending host->device request the driver has queued. The
    /// driver returns the full `ExtenderPendingOutputHeader` plus payload as a
    /// single struct output (no scalar outputs); an empty struct means nothing
    /// is pending.
    pub fn get_pending_output(
        &self,
        device_id: u32,
    ) -> Result<Option<PendingRequest>, ClientError> {
        let scalars_in = [device_id as u64];
        let mut buf = vec![0u8; 65536];
        let mut buf_len = buf.len();

        let kr = unsafe {
            IOConnectCallMethod(
                self.connect,
                SELECTOR_GET_PENDING_OUTPUT,
                scalars_in.as_ptr(),
                scalars_in.len() as u32,
                ptr::null(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                buf.as_mut_ptr() as *mut c_void,
                &mut buf_len,
            )
        };

        if kr != KERN_SUCCESS {
            return Err(ClientError::IOKit {
                code: kr,
                message: "get_pending_output failed".to_owned(),
            });
        }

        // Empty struct → nothing pending.
        if buf_len == 0 {
            return Ok(None);
        }
        if buf_len < PENDING_OUTPUT_HEADER_SIZE {
            return Err(ClientError::IOKit {
                code: -1,
                message: format!(
                    "get_pending_output: short header {} < {}",
                    buf_len, PENDING_OUTPUT_HEADER_SIZE
                ),
            });
        }

        // Parse header (little-endian, host byte order — IOKit shares memory in
        // host endianness).
        let dev_id = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
        let endpoint = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
        let data_length = u32::from_ne_bytes(buf[8..12].try_into().unwrap()) as usize;
        let _request_type = u32::from_ne_bytes(buf[12..16].try_into().unwrap());
        let request_id = u32::from_ne_bytes(buf[16..20].try_into().unwrap());
        // bytes 20..24 are reserved.

        let payload_end = PENDING_OUTPUT_HEADER_SIZE + data_length;
        if payload_end > buf_len {
            return Err(ClientError::IOKit {
                code: -1,
                message: format!(
                    "get_pending_output: truncated payload {} > {}",
                    payload_end, buf_len
                ),
            });
        }

        let mut data = buf;
        data.truncate(payload_end);
        let payload = data.split_off(PENDING_OUTPUT_HEADER_SIZE);

        Ok(Some(PendingRequest {
            device_id: dev_id,
            endpoint,
            request_id,
            data: payload,
        }))
    }

    /// Configure a virtual device with descriptor data.
    ///
    /// # Arguments
    /// * `device_id` - The target device ID.
    /// * `config_type` - One of the `CONFIG_*` constants.
    /// * `data` - The configuration payload (e.g., HID report descriptor bytes).
    pub fn configure_device(
        &self,
        device_id: u32,
        config_type: u32,
        data: &[u8],
    ) -> Result<(), ClientError> {
        let scalars = [device_id as u64, config_type as u64];
        let kr = unsafe {
            IOConnectCallMethod(
                self.connect,
                SELECTOR_CONFIGURE_DEVICE,
                scalars.as_ptr(),
                scalars.len() as u32,
                data.as_ptr() as *const c_void,
                data.len(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        check_kr(kr, "configure_device")
    }

    /// Get the count of active virtual devices.
    pub fn device_count(&self) -> Result<u32, ClientError> {
        let mut output = [0u64; 1];
        let mut output_cnt = 1u32;
        let kr = unsafe {
            IOConnectCallScalarMethod(
                self.connect,
                SELECTOR_GET_DEVICE_COUNT,
                ptr::null(),
                0,
                output.as_mut_ptr(),
                &mut output_cnt,
            )
        };
        check_kr(kr, "device_count")?;
        Ok(output[0] as u32)
    }
}

impl Drop for MacOSVhciDriver {
    fn drop(&mut self) {
        unsafe {
            IOServiceClose(self.connect);
        }
    }
}

// Safety: The IOKit connection handle is thread-safe when used with
// synchronized IOConnectCall* methods (each call is atomic from the
// driver's perspective).
unsafe impl Send for MacOSVhciDriver {}
unsafe impl Sync for MacOSVhciDriver {}

// ---------------------------------------------------------------------------
// PendingRequest
// ---------------------------------------------------------------------------

/// A pending request from a virtual device that needs to be sent over the
/// network as a USB/IP URB.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// The device that generated this request.
    pub device_id: u32,
    /// The USB endpoint the request targets.
    pub endpoint: u32,
    /// Driver-assigned request ID for correlating responses.
    pub request_id: u32,
    /// The request payload data.
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// USB device class detection
// ---------------------------------------------------------------------------

/// Determine the virtual device type from USB device/interface class codes.
///
/// Checks the device class first, then falls back to interface classes.
/// Defaults to HID if no recognized class is found (most common for
/// consumer USB devices).
pub fn device_type_from_usb(device_class: u8, interface_classes: &[u8]) -> u32 {
    // Check device class first.
    match device_class {
        0x03 => return DEVICE_TYPE_HID,
        0x08 => return DEVICE_TYPE_STORAGE,
        0x02 | 0x0A => return DEVICE_TYPE_SERIAL, // CDC
        0x01 => return DEVICE_TYPE_AUDIO,
        _ => {}
    }

    // Check for network as a special case first: composite device (0xEF) with
    // CDC interfaces (0x02) indicates CDC-ECM/NCM network device.
    if device_class == 0xEF {
        for &ic in interface_classes {
            if ic == 0x02 {
                return DEVICE_TYPE_NETWORK;
            }
        }
    }

    // Check interface classes.
    for &ic in interface_classes {
        match ic {
            0x03 => return DEVICE_TYPE_HID,
            0x08 => return DEVICE_TYPE_STORAGE,
            0x02 | 0x0A => return DEVICE_TYPE_SERIAL,
            0x01 => return DEVICE_TYPE_AUDIO,
            0xE0 => return DEVICE_TYPE_NETWORK, // Wireless controller
            _ => {}
        }
    }

    // Default to HID as the most common consumer device type.
    DEVICE_TYPE_HID
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check a `kern_return_t` and convert non-success to `ClientError`.
fn check_kr(kr: kern_return_t, operation: &str) -> Result<(), ClientError> {
    if kr == KERN_SUCCESS {
        Ok(())
    } else {
        Err(ClientError::IOKit {
            code: kr,
            message: format!("{operation} failed"),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_type_from_usb_device_class() {
        assert_eq!(device_type_from_usb(0x03, &[]), DEVICE_TYPE_HID);
        assert_eq!(device_type_from_usb(0x08, &[]), DEVICE_TYPE_STORAGE);
        assert_eq!(device_type_from_usb(0x02, &[]), DEVICE_TYPE_SERIAL);
        assert_eq!(device_type_from_usb(0x0A, &[]), DEVICE_TYPE_SERIAL);
        assert_eq!(device_type_from_usb(0x01, &[]), DEVICE_TYPE_AUDIO);
    }

    #[test]
    fn test_device_type_from_usb_interface_class() {
        assert_eq!(device_type_from_usb(0x00, &[0x03]), DEVICE_TYPE_HID);
        assert_eq!(device_type_from_usb(0x00, &[0x08]), DEVICE_TYPE_STORAGE);
        assert_eq!(device_type_from_usb(0x00, &[0x02]), DEVICE_TYPE_SERIAL);
        assert_eq!(device_type_from_usb(0x00, &[0x01]), DEVICE_TYPE_AUDIO);
    }

    #[test]
    fn test_device_type_from_usb_composite_network() {
        // Composite device (0xEF) with CDC interface (0x02) -> network
        assert_eq!(device_type_from_usb(0xEF, &[0x02]), DEVICE_TYPE_NETWORK);
    }

    #[test]
    fn test_device_type_from_usb_default_hid() {
        // Unknown class codes default to HID.
        assert_eq!(device_type_from_usb(0xFF, &[0xFF]), DEVICE_TYPE_HID);
        assert_eq!(device_type_from_usb(0x00, &[]), DEVICE_TYPE_HID);
    }

    #[test]
    fn test_device_type_first_match_wins() {
        // When multiple interface classes are present, the first recognized one wins.
        assert_eq!(
            device_type_from_usb(0x00, &[0x03, 0x08]),
            DEVICE_TYPE_HID
        );
        assert_eq!(
            device_type_from_usb(0x00, &[0x08, 0x03]),
            DEVICE_TYPE_STORAGE
        );
    }

    #[test]
    fn test_device_type_device_class_takes_priority() {
        // Device class should take priority over interface classes.
        assert_eq!(
            device_type_from_usb(0x03, &[0x08]),
            DEVICE_TYPE_HID
        );
    }

    #[test]
    fn test_selector_values() {
        assert_eq!(SELECTOR_CREATE_DEVICE, 0);
        assert_eq!(SELECTOR_DESTROY_DEVICE, 1);
        assert_eq!(SELECTOR_SUBMIT_INPUT, 2);
        assert_eq!(SELECTOR_GET_PENDING_OUTPUT, 3);
        assert_eq!(SELECTOR_GET_DEVICE_COUNT, 4);
        assert_eq!(SELECTOR_CONFIGURE_DEVICE, 5);
    }

    #[test]
    fn test_pending_request_struct() {
        let req = PendingRequest {
            device_id: 1,
            endpoint: 2,
            request_id: 42,
            data: vec![0xDE, 0xAD],
        };
        assert_eq!(req.device_id, 1);
        assert_eq!(req.endpoint, 2);
        assert_eq!(req.request_id, 42);
        assert_eq!(req.data, vec![0xDE, 0xAD]);
    }

    /// Verify that the IOKit type sizes match expected C ABI sizes.
    #[test]
    fn test_iokit_type_sizes() {
        assert_eq!(std::mem::size_of::<io_service_t>(), 4);
        assert_eq!(std::mem::size_of::<io_connect_t>(), 4);
        assert_eq!(std::mem::size_of::<io_object_t>(), 4);
        assert_eq!(std::mem::size_of::<mach_port_t>(), 4);
        assert_eq!(std::mem::size_of::<kern_return_t>(), 4);
    }

    /// Test that opening the driver fails gracefully when not available.
    /// This test requires macOS but NOT the driver to be loaded.
    #[test]
    #[ignore]
    fn test_open_driver_not_available() {
        // On a macOS system without the DriverKit extension, this should
        // return VhciNotAvailable, not panic.
        let result = MacOSVhciDriver::new();
        assert!(
            result.is_err(),
            "expected error when driver is not loaded"
        );
    }
}
