# Extender

Share USB devices over the network. Plug a device into one machine, use it on another.

Built in Rust. Works on macOS and Linux. Wire-compatible with the Linux kernel's USB/IP implementation.

## What it does

```
┌─────────────────┐         TCP/IP          ┌─────────────────┐
│   Your Mac       │◄──────────────────────►│   Linux Server   │
│                  │     USB/IP protocol     │                  │
│  USB device      │                         │  Device appears  │
│  plugged in here │                         │  as if local     │
└─────────────────┘                         └─────────────────┘
```

Export a USB device from any machine. Import it on another. The remote machine sees it as a locally connected device — drivers load, applications work, no changes needed.

## Quick Start

```bash
# On the machine with the USB device (server)
extender daemon --listen 0.0.0.0
extender bind -b 1-2.4          # export the device

# On the remote machine (client) — using Linux kernel usbip
sudo modprobe vhci_hcd
usbip list -r 192.168.1.50      # see exported devices
sudo usbip attach -r 192.168.1.50 -b 1-2.4   # import it
lsusb                            # device appears locally
```

## Features

- **Cross-platform server** — export USB devices from macOS or Linux
- **Linux client** — import devices via kernel vhci_hcd (wire-compatible with `usbip`)
- **API-first** — JSON-RPC over Unix socket, build any UI on top
- **macOS menu bar app** — native SwiftUI interface
- **Single binary** — `extender` does everything (daemon, CLI, server, client)
- **Small** — 3-4 MB release binary
- **Fast** — sub-millisecond URB forwarding, USB 3.0 SuperSpeed support

## Installation

### Pre-built binaries

Download from [GitHub Releases](https://github.com/calibrae/extender/releases).

### Build from source

```bash
cd extender
cargo build --release
# Binary at target/release/extender
```

Requires:
- Rust 1.75+ (stable)
- libusb 1.0 (`brew install libusb` on macOS, `apt install libusb-1.0-0-dev` on Linux)

### macOS app

```bash
cd extender-macos/ExtenderApp
swift build
# Run the menu bar app
open .build/debug/ExtenderApp
```

## Usage

### Start the daemon

```bash
extender daemon                          # localhost only (default)
extender daemon --listen 0.0.0.0         # accept network connections
extender daemon --port 9999              # custom port (default: 3240)
```

### List USB devices

```bash
extender list -l                         # local devices
extender list -r 192.168.1.50            # query remote server
extender --format json list -l           # JSON output
```

### Export (bind) a device

```bash
extender bind -b 1-2.4                   # export by bus ID
extender status                          # see what's exported
extender unbind -b 1-2.4                 # stop exporting
```

### Import a device (Linux client)

```bash
sudo modprobe vhci_hcd                   # load kernel module
sudo usbip attach -r <server> -b 1-2.4  # import device
sudo usbip detach -p 0                  # detach when done
```

## Architecture

```
extender/                  Rust workspace
├── extender-protocol/     USB/IP v1.1.1 wire format
├── extender-server/       Device export + URB forwarding
├── extender-client/       Device import (Linux vhci_hcd)
├── extender-daemon/       Service orchestration + JSON-RPC API
├── extender-api/          Shared API types
└── extender-cli/          Command-line interface

extender-macos/            Native macOS menu bar app (SwiftUI)
```

The daemon runs both server and client — any machine can export and import simultaneously.

## Protocol Compatibility

Extender implements USB/IP protocol v1.1.1 as specified by the [Linux kernel documentation](https://docs.kernel.org/usb/usbip_protocol.html). It interoperates with:

- Linux kernel `usbipd` (server)
- Linux kernel `vhci_hcd` + `usbip` (client)

## Security

- Default listen address is `127.0.0.1` (localhost only)
- Network exposure requires explicit `--listen 0.0.0.0`
- No authentication in v0.1 (same as Linux kernel usbip) — use a VPN or SSH tunnel
- TLS planned for v0.2
- USB descriptor validation, transfer buffer size caps, bus ID format validation
- Privilege dropping after port bind

## Requirements

- **Server (export):** macOS 13+ or Linux. Needs root/sudo to claim USB devices.
- **Client (import):** Linux with `vhci_hcd` kernel module. macOS client planned for v0.2.

## License

MIT OR Apache-2.0
