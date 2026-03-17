# Extender DriverKit Extension

Virtual USB device driver for macOS using Apple's DriverKit framework.

## What it does

Creates virtual USB devices on macOS when importing remote devices via USB/IP:
- **Block Storage** — Remote USB drives appear in Finder/Disk Utility
- **HID** — Remote keyboards/mice work as local input devices
- **Serial** — Remote serial adapters appear as `/dev/tty.*`
- **Networking** — Remote USB Ethernet adapters create network interfaces

## Architecture

```
Remote USB Device → USB/IP → Extender Daemon → IOUserClient → DriverKit Extension → macOS
```

The daemon handles USB/IP protocol and SCSI/HID/Serial translation.
The DriverKit extension presents the virtual device to macOS.
Communication between daemon and driver via IOUserClient ExternalMethod calls.

## Building

Requires Xcode 15+ and DriverKit entitlements from Apple.

This extension must be embedded in the Extender.app bundle at:
`Extender.app/Contents/Library/SystemExtensions/com.calibrae.extender.driver.systemextension`

## Entitlements Required

- `com.apple.developer.driverkit`
- `com.apple.developer.driverkit.family.scsi-controller`
- `com.apple.developer.driverkit.family.block-storage-device`
- `com.apple.developer.driverkit.family.hid.device`
- `com.apple.developer.driverkit.family.networking`
- `com.apple.developer.driverkit.family.serial`
- `com.apple.developer.driverkit.userclient-access`
