# Extender for macOS

Native macOS app wrapping the Extender USB/IP daemon with:
- Menu bar UI for device management
- DriverKit system extension for virtual HID device import
- PTY serial bridge for CDC/ACM device import
- Code signing, notarization, and distribution

## Architecture

```
ExtenderApp/               ← macOS menu bar app (SwiftUI)
  ExtenderBridge/          ← Swift ↔ Rust daemon communication (JSON-RPC over Unix socket)
  ExtenderMenuBar/         ← Menu bar UI
  ExtenderSettings/        ← Preferences window

ExtenderDriverKit/         ← DriverKit system extension
  VirtualHIDDevice/        ← Virtual keyboard/mouse/gamepad
  ExtensionManager/        ← Install/approve/activate lifecycle

Resources/
  extender                 ← Embedded Rust daemon binary
  extender.service.plist   ← launchd plist
```

## Communication

The Swift app talks to the Rust daemon via the same Unix socket JSON-RPC API
that the CLI uses. No FFI — just socket communication.

## Building

Requires:
- Xcode 15+
- Apple Developer account with DriverKit entitlements
- macOS 13+ deployment target
