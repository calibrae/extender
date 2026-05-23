# Extender

Share USB devices over the network. Plug a device into one machine, use it on another.

Built in Rust. Wire-compatible with the Linux kernel's USB/IP implementation.

```
┌─────────────────┐                          ┌─────────────────┐
│  Linux / macOS  │      USB/IP over TCP     │  Linux / macOS  │
│                  │ ◄──────────────────────► │                 │
│  USB device      │       (optional TLS)     │ Device appears  │
│  plugged in here │                          │ as if local     │
└─────────────────┘                          └─────────────────┘
```

## Current Support Matrix

| Direction | Server (export) | Client (import) | Status |
|---|---|---|---|
| Linux → Linux | ✅ libusb | ✅ kernel `vhci_hcd` | works out of the box |
| Linux → Windows | ✅ libusb | ✅ usbip-win2 UDE | works (Microsoft-signed driver) |
| Linux → macOS | ✅ libusb | ⚠️ DriverKit dext | **dext is built but blocked by Sequoia policy — see below** |
| macOS → Linux | ⚠️ DriverKit dext | ✅ kernel `vhci_hcd` | dext partial; needs `transport.usb` for non-HID devices |
| macOS → macOS | both ⚠️ | both ⚠️ | blocked on both sides today |
| Windows → anything | ❌ | ✅ | not implemented |

**What works today, end-to-end, without caveats:** Linux ↔ Linux, Linux ↔ Windows. The macOS work is shipped as code (signed, notarized, builds clean), but doesn't load on stock macOS Sequoia without [extra steps](#macos-current-state-and-walls).

## What it does (where it works)

```bash
# On the machine with the USB device (server)
sudo extender daemon --listen 0.0.0.0
sudo extender bind -b 1-2.4

# On the remote machine (client)
extender discover                              # find servers on LAN
extender list -r 192.168.1.50                  # see exported devices
sudo usbip attach -r 192.168.1.50 -b 1-2.4    # import (Linux kernel client)
```

## Features

- **Wire-compatible** with Linux kernel `usbipd` and `vhci_hcd` (USB/IP protocol v1.1.1)
- **TLS encryption** — mutual TLS with `extender tls-gen` for easy cert setup
- **Auto-discovery** — mDNS/DNS-SD finds servers on the LAN
- **Device ACLs** — allow/deny by VID:PID pattern
- **Auto-reconnect** — exponential backoff with session persistence on server
- **API-first** — JSON-RPC API, build any UI on top
- **Single binary** — `extender` does everything: daemon, CLI, server, client
- **Small & fast** — 3–4 MB binary, sub-millisecond URB forwarding, USB 3.0 SuperSpeed

## Installation

### Pre-built binaries

Download from [GitHub Releases](https://github.com/calibrae/extender/releases):

| Platform | File |
|---|---|
| Linux (x86_64) | `extender-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) | `extender-vX.Y.Z-x86_64-pc-windows-msvc.zip` |
| macOS (Apple Silicon) | `extender-vX.Y.Z-aarch64-apple-darwin.tar.gz` (CLI only) |

### Build from source

```bash
cd extender
cargo build --release
```

Requires Rust 1.75+ and libusb 1.0:
- Linux: `apt install libusb-1.0-0-dev`
- macOS: `brew install libusb`
- Windows: `vcpkg install libusb:x64-windows`

## Usage

### Daemon

```bash
extender daemon                                # localhost only
extender daemon --listen 0.0.0.0               # accept network connections
extender daemon --tls-cert cert.pem --tls-key key.pem
```

### Device management

```bash
extender list -l                               # local USB devices (Linux server)
extender list -r 192.168.1.50                  # remote server
extender bind -b 1-2.4                         # export a device
extender unbind -b 1-2.4                       # stop exporting
extender status                                # overview
extender discover                              # find servers on LAN
```

### Import (Linux client)

```bash
sudo modprobe vhci_hcd
sudo usbip attach -r <server> -b 1-2.4
lsusb                                          # device appears locally
sudo usbip detach -p 0
```

### TLS

```bash
extender tls-gen                               # generates CA + server + client certs
extender daemon --tls-cert server-cert.pem --tls-key server-key.pem
extender list -r server --tls --tls-ca ca.pem
```

## Architecture

```
extender/                    Rust workspace
├── extender-protocol/       USB/IP v1.1.1 wire format
├── extender-server/         Device export, URB forwarding, TLS
├── extender-client/         Device import, vhci_hcd, mDNS discovery
├── extender-daemon/         Daemon, JSON-RPC API, config, ACLs
├── extender-api/            Shared API types
└── extender-cli/            CLI binary

extender-macos/              DriverKit extension + menu bar app (Swift)
extender-windows/            Windows system tray app (WPF/C#)
```

## Configuration

Config at `~/.config/extender/config.toml` (Linux/macOS) or `%APPDATA%\Extender\config.toml` (Windows):

```toml
[server]
listen_address = "127.0.0.1"
port = 3240
tls_cert = "~/.config/extender/tls/server-cert.pem"
tls_key = "~/.config/extender/tls/server-key.pem"

[security]
allowed_devices = []                          # empty = all allowed
denied_devices = ["0bda:*"]                   # block by VID:PID pattern

[daemon]
log_level = "info"
```

Environment variables: `EXTENDER_PORT`, `EXTENDER_HOST`, `EXTENDER_SOCKET`, `EXTENDER_LOG_LEVEL`.

## Security

- Default listen address: `127.0.0.1` (localhost only)
- TLS with mutual authentication (mTLS) for network deployments
- Device ACLs with VID:PID wildcard patterns (deny overrides allow)
- Bus ID format validation on all network input
- Transfer buffer capped at 1 MB
- Privilege dropping after port bind
- Sanitized error messages to API clients

## macOS: current state and walls

The macOS side of extender is **fully implemented** (5 DriverKit virtual device classes for the client side, IOUSBHostDevice-based export-side scaffold for the server side, Swift menu-bar host app, full async URB forwarding in the Rust daemon). It builds clean, signs with Developer ID, passes Apple notarization, and ships in a stapled .app bundle.

**It does not load on stock macOS Sequoia (26+).**

This isn't a code bug. macOS Sequoia tightened DriverKit dext activation policy: any dext activated programmatically via `OSSystemExtensionRequest` (which is the only way to load a software-only dext that doesn't match specific hardware) is silently rejected with `sysextd: no policy, cannot allow apps outside /Applications` unless the Mac has one of:

1. **MDM enrollment** with a User Approved MDM connection that includes a System Extension Policy payload pre-authorizing our team (`XJQQCN392F`). Configuration Profiles sideloaded outside MDM are rejected with "must originate from a user approved MDM server".
2. **Reduced Security mode** with "Allow user management of kernel extensions from identified developers" enabled. This requires a physical Recovery boot (hold power button on Apple Silicon) — there is no remote or programmatic equivalent.

Tested on three clean Sequoia machines (calimba/MacBook Air, speedwagon/Mac mini SIP-off, giorno/Mac mini SIP-on, all running 26.x) — same rejection on all. The "no policy" error is misleading: the app *is* in /Applications, properly signed, notarized, stapled. The policy refers to MDM-pushed authorization, not the path.

**If you want to use extender on macOS today:**

1. Boot to Recovery mode (shut down, hold power until "Loading startup options")
2. Options → continue → Startup Security Utility
3. Select your boot disk → Security Policy → **Reduced Security**
4. Check **"Allow user management of kernel extensions from identified developers"**
5. Restart → install the notarized .app from /Applications → approve in System Settings → General → Login Items & Extensions → Driver Extensions

We hope Apple relaxes this in a future release or grants extender's team an allowlist. Until then macOS support is "developer/sysadmin tier" rather than "consumer".

**Why consumer DriverKit dexts (like Promise Pegasus Thunderbolt) work without this dance:** they use hardware-match personalities — when matching hardware plugs in, macOS auto-prompts the user. Extender provides *virtual* USB devices created on demand by the daemon, with no physical hardware to trigger matching. The hardware-trigger path doesn't apply to our architecture.

### Entitlements granted

- `com.apple.developer.driverkit` ✅ (development tier, March 2026)
- All `com.apple.developer.driverkit.family.*` ✅
- `com.apple.developer.driverkit.transport.hid` ✅
- `com.apple.developer.driverkit.transport.usb` ✅ (development tier; distribution tier requires additional review)
- `com.apple.developer.system-extension.install` ✅

## Protocol Compatibility

Implements USB/IP protocol v1.1.1 per the [Linux kernel specification](https://docs.kernel.org/usb/usbip_protocol.html). Interoperates with:

- Linux kernel `usbipd` (server) and `vhci_hcd` + `usbip` (client)
- usbip-win2 UDE driver on Windows (Microsoft-signed)
- Any USB/IP v1.1.1 compliant implementation

## Requirements

| | Server (export) | Client (import) |
|---|---|---|
| **Linux** | Any with libusb | `vhci_hcd` kernel module. Just works.™ |
| **Windows** | 10 21H2+ with libusb | usbip-win2 UDE driver (Microsoft-signed) |
| **macOS** | 13+, DriverKit dext (Reduced Security required on 26+) | 13+, DriverKit dext (Reduced Security required on 26+) |

## License

MIT OR Apache-2.0
