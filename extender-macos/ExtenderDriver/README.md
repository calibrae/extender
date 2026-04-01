# Extender DriverKit Extension

Virtual USB device driver for macOS using Apple's DriverKit framework.

## What it does

Creates virtual USB devices on macOS when importing remote devices via USB/IP:
- **HID** — Remote keyboards, mice, gamepads work as local input devices
- **Block Storage** — Remote USB drives appear in Finder/Disk Utility
- **Serial** — Remote serial adapters appear as `/dev/tty.*`
- **Networking** — Remote USB Ethernet adapters create network interfaces
- **Audio** — Remote USB audio devices appear in Sound preferences

## Architecture

```
Remote USB Device → USB/IP → Extender Daemon → IOUserClient → DriverKit Extension → macOS
```

### Components

- **ExtenderDriver** — Top-level IOService managing up to 32 virtual devices
- **ExtenderUserClient** — IPC bridge, dispatches ExternalMethod calls from the Rust daemon
- **ExtenderProtocol.h** — Shared IPC protocol definitions (selectors, config types, structures)
- **ExtenderVirtualHID** — IOUserHIDDevice subclass for keyboards, mice, gamepads
- **ExtenderVirtualStorage** — Block storage device with pending I/O queue
- **ExtenderVirtualNetwork** — IOUserNetworkEthernet subclass for USB Ethernet adapters
- **ExtenderVirtualSerial** — Serial port with ring buffer I/O
- **ExtenderVirtualAudio** — IOUserAudioDevice subclass for USB audio

### IPC Protocol

The daemon communicates with the driver via 6 ExternalMethod selectors:

| Selector | Name | Input | Output |
|----------|------|-------|--------|
| 0 | kCreateDevice | deviceType, deviceId | — |
| 1 | kDestroyDevice | deviceId | — |
| 2 | kSubmitInput | deviceId, endpoint, length + data | — |
| 3 | kGetPendingOutput | deviceId | pending request data |
| 4 | kGetDeviceCount | — | count |
| 5 | kConfigureDevice | deviceId, configType + config data | — |

## Building

Requires Xcode 15+ and DriverKit entitlements from Apple.

This extension must be embedded in the Extender.app bundle at:
`Extender.app/Contents/Library/SystemExtensions/com.calibrae.extender.driver.systemextension`

## Entitlements Required

- `com.apple.developer.driverkit`
- `com.apple.developer.driverkit.transport.hid`
- `com.apple.developer.driverkit.family.hid.device`
- `com.apple.developer.driverkit.family.hid.eventservice`
- `com.apple.developer.driverkit.family.scsi-controller`
- `com.apple.developer.driverkit.family.networking`
- `com.apple.developer.driverkit.family.serial`
- `com.apple.developer.driverkit.family.audio`
- `com.apple.developer.driverkit.family.midi`
- `com.apple.developer.driverkit.userclient-access`
