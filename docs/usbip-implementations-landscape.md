# USB/IP Implementations Landscape

## Existing Implementations

### Server Implementations

| Implementation | Language | Platform | Notes |
|---|---|---|---|
| usbip-host.ko | C | Linux Kernel | USB device driver; no UDC/device function |
| usbip-vudc.ko | C | Linux Kernel | Virtual UDC with USB Gadget support |
| usbip_windows | C | Windows | Signed driver (signed by ReactOS) |
| dorssel/usbipd-win | C | Windows | VirtualBox-based; signed driver |
| cezanne/usbip-win | C | Windows | Self-written/signed stub & vhci drivers |
| **jiegec/usbip** | **Rust** | **Cross-platform** | libusb backend; HID keyboard, CDC serial examples |
| Sawchord/usbip-device | Rust | Cross-platform | Serial echo, mouse emulation |
| ellerh/softfido | Rust | Cross-platform | FIDO2/U2F authenticator |
| MarkOstis/USBIP-Virtual-USB-Device | Python | Cross-platform | Keyboard and mouse devices |
| canokey-usbip | C | Cross-platform | FIDO2, OpenPGP V3.4, PIV, HOTP/TOTP |
| chegewara/esp32-usbip-poc | C++ | ESP32 | WiFi-accessible serial devices |

### Client Implementations

| Implementation | Language | Platform | Notes |
|---|---|---|---|
| vhci-hcd.ko | C | Linux Kernel | Virtual HCI driver |
| cezanne/usbip-win | C | Windows | Self-signed virtual HCI driver |
| forensix/libusbip | C | Cross-platform | C library on top of libusb |

### User Space Tools

- Linux usbip/usbipd CLI tools
- alunux/usbip-service-discovery (GUI)
- USBIPManager (GUI)

---

## Key Rust Implementation: jiegec/usbip

- **Crate:** https://crates.io/crates/usbip
- **License:** MIT
- **Features:** USB/IP server to simulate USB devices and share real USB devices
- **Backend:** libusb (works on Linux, macOS, Windows, OpenBSD/NetBSD, Haiku, Solaris)
- **Examples:** HID Keyboard, CDC ACM Serial, Host Server
- **Client requirement:** Linux with USB/IP support (vhci_hcd module)
- **Tested:** Sharing CCID SmartCard from macOS to Linux

### Notable Gap

- **No macOS client** — macOS lacks a vhci_hcd equivalent kernel module
- **No standalone cross-platform client** — all existing clients rely on OS-specific kernel drivers
- There is no existing Go implementation of USB/IP

---

## Platform Support Summary

| Platform | Server (Export) | Client (Import) |
|----------|----------------|-----------------|
| Linux    | Native kernel module | Native kernel module (vhci_hcd) |
| Windows  | Third-party drivers | Third-party drivers (usbip-win) |
| macOS    | Via libusb (jiegec/usbip Rust) | **NOT SUPPORTED** |

## Sources

- https://github.com/usbip/implementations
- https://github.com/jiegec/usbip
- https://crates.io/crates/usbip
- https://crates.io/crates/usbip-device
