# Extender

Share USB devices over the network. Plug a device into one machine, use it on another.

Built in Rust. Works on macOS, Linux, and Windows. Wire-compatible with the Linux kernel's USB/IP implementation.

## What it does

```
┌─────────────────┐                         ┌─────────────────┐
│   macOS / Win    │     USB/IP over TCP     │   Linux / Mac   │
│                  │◄───────────────────────►│                 │
│  USB device      │   (optional TLS)        │  Device appears │
│  plugged in here │                         │  as if local    │
└─────────────────┘                         └─────────────────┘
```

Export a USB device from any machine. Import it on another. The remote machine sees it as a locally connected device — drivers load, applications work, no changes needed.

## Quick Start

```bash
# On the machine with the USB device (server)
sudo extender daemon --listen 0.0.0.0
sudo extender bind -b 1-2.4

# On the remote machine (client)
extender discover                              # find servers on LAN
extender list -r 192.168.1.50                  # see exported devices
sudo usbip attach -r 192.168.1.50 -b 1-2.4    # import it (Linux)
```

## Features

- **Three-platform support** — macOS, Linux, and Windows server + CLI
- **Wire-compatible** — interoperates with Linux kernel `usbipd` and `vhci_hcd`
- **TLS encryption** — mutual TLS with `extender tls-gen` for easy cert setup
- **Auto-discovery** — mDNS/DNS-SD finds servers on the LAN automatically
- **Device ACLs** — allow/deny devices by VID:PID pattern
- **Auto-reconnect** — exponential backoff with session persistence on server
- **API-first** — JSON-RPC API, build any UI on top
- **Native apps** — macOS menu bar (SwiftUI), Windows system tray (WPF)
- **Single binary** — `extender` does everything: daemon, CLI, server, client
- **Small & fast** — 3-4 MB binary, sub-millisecond URB forwarding, USB 3.0 SuperSpeed

## Installation

### Pre-built binaries

Download from [GitHub Releases](https://github.com/calibrae/extender/releases):

| Platform | File |
|---|---|
| macOS (Apple Silicon) | `Extender-vX.Y.Z-macOS.dmg` (signed + notarized) |
| macOS CLI only | `extender-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| Linux (x86_64) | `extender-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
| Windows (x86_64) | `extender-vX.Y.Z-x86_64-pc-windows-msvc.zip` |

### Build from source

```bash
cd extender
cargo build --release
```

Requires Rust 1.75+ and libusb 1.0:
- macOS: `brew install libusb`
- Linux: `apt install libusb-1.0-0-dev`
- Windows: `vcpkg install libusb:x64-windows`

## Usage

### Daemon

```bash
extender daemon                                # localhost only
extender daemon --listen 0.0.0.0               # accept network connections
extender daemon --tls-cert cert.pem --tls-key key.pem   # with TLS
```

### Device management

```bash
extender list -l                               # local USB devices
extender list -r 192.168.1.50                  # remote server
extender list -r server --tls --tls-ca ca.pem  # remote over TLS
extender bind -b 1-2.4                         # export a device
extender unbind -b 1-2.4                       # stop exporting
extender status                                # overview
extender discover                              # find servers on LAN
```

### Import a device (Linux client)

```bash
sudo modprobe vhci_hcd
sudo usbip attach -r <server> -b 1-2.4
lsusb                                          # device appears locally
sudo usbip detach -p 0                         # disconnect
```

### TLS setup

```bash
extender tls-gen                               # generates CA + server + client certs
extender daemon --tls-cert server-cert.pem --tls-key server-key.pem
extender list -r server --tls --tls-ca ca.pem  # clients verify server
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

extender-macos/              macOS menu bar app (SwiftUI)
extender-windows/            Windows system tray app (WPF/C#)
```

The daemon runs both server and client — any machine can export and import simultaneously.

## Configuration

Config file at `~/.config/extender/config.toml` (Linux/macOS) or `%APPDATA%\Extender\config.toml` (Windows):

```toml
[server]
listen_address = "127.0.0.1"
port = 3240
tls_cert = "~/.config/extender/tls/server-cert.pem"
tls_key = "~/.config/extender/tls/server-key.pem"

[security]
allowed_devices = []              # empty = all allowed
denied_devices = ["0bda:*"]       # block by VID:PID pattern

[daemon]
log_level = "info"
```

Environment variables: `EXTENDER_PORT`, `EXTENDER_HOST`, `EXTENDER_SOCKET`, `EXTENDER_LOG_LEVEL`.

## Security

- Default listen address: `127.0.0.1` (localhost only)
- TLS with mutual authentication (mTLS) for network deployments
- Device ACLs with VID:PID wildcard patterns (deny overrides allow)
- Bus ID format validation on all network input
- Transfer buffer capped at 1 MB (prevents memory exhaustion)
- Privilege dropping after port bind
- Sanitized error messages to API clients
- OWASP-audited codebase

## Protocol Compatibility

Implements USB/IP protocol v1.1.1 per the [Linux kernel specification](https://docs.kernel.org/usb/usbip_protocol.html). Interoperates with:

- Linux kernel `usbipd` (server)
- Linux kernel `vhci_hcd` + `usbip` (client)
- Any USB/IP v1.1.1 compliant implementation

## Requirements

| | Server (export) | Client (import) |
|---|---|---|
| **macOS** | 13+ with root | Planned (DriverKit) |
| **Linux** | Any with libusb | `vhci_hcd` kernel module |
| **Windows** | 10 21H2+ with libusb | Planned (usbip-win2 UDE) |

## License

MIT OR Apache-2.0
