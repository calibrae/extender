import Foundation

/// API types matching the Rust daemon's JSON responses.
public struct DeviceInfo: Codable, Identifiable, Hashable {
    public let bus_id: String
    public let vendor_id: UInt16
    public let product_id: UInt16
    public let manufacturer: String?
    public let product: String?
    public let device_class: UInt8
    public let speed: String
    public let is_bound: Bool

    public var id: String { bus_id }

    public var vidPid: String {
        String(format: "%04x:%04x", vendor_id, product_id)
    }

    public var displayName: String {
        if let product = product {
            return product
        }
        return vidPid
    }

    public var speedDisplay: String {
        switch speed {
        case "low": return "1.5 Mbps"
        case "full": return "12 Mbps"
        case "high": return "480 Mbps"
        case "super": return "5 Gbps"
        default: return speed
        }
    }
}

public struct ExportedDeviceInfo: Codable, Identifiable {
    public let bus_id: String
    public let vendor_id: UInt16
    public let product_id: UInt16
    public let manufacturer: String?
    public let product: String?
    public let device_class: UInt8
    public let speed: String
    public let num_clients: UInt32

    public var id: String { bus_id }
}

public struct DaemonStatus: Codable {
    public let version: String
    public let uptime_secs: UInt64
    public let exported_devices: UInt32
    public let imported_devices: UInt32
    public let active_connections: UInt32

    public var uptimeDisplay: String {
        let hours = uptime_secs / 3600
        let minutes = (uptime_secs % 3600) / 60
        let seconds = uptime_secs % 60
        if hours > 0 {
            return "\(hours)h \(minutes)m"
        } else if minutes > 0 {
            return "\(minutes)m \(seconds)s"
        }
        return "\(seconds)s"
    }
}

/// A remote server discovered via mDNS or manually added.
public struct RemoteServer: Identifiable, Hashable {
    public let id: String  // host:port
    public let host: String
    public let port: UInt16
    public let name: String?  // mDNS service name
}

/// A device available on a remote server.
public struct RemoteDeviceInfo: Codable, Identifiable {
    public let bus_id: String
    public let vendor_id: UInt16
    public let product_id: UInt16
    public let device_class: UInt8
    public let speed: String

    public var id: String { bus_id }

    public var vidPid: String {
        String(format: "%04x:%04x", vendor_id, product_id)
    }

    public var speedDisplay: String {
        switch speed {
        case "low": return "1.5 Mbps"
        case "full": return "12 Mbps"
        case "high": return "480 Mbps"
        case "super": return "5 Gbps"
        default: return speed
        }
    }

    public var deviceClassName: String {
        switch device_class {
        case 0x03: return "HID"
        case 0x08: return "Storage"
        case 0x02, 0x0A: return "Serial"
        case 0x01: return "Audio"
        case 0x09: return "Hub"
        case 0xE0: return "Wireless"
        case 0xEF: return "Misc"
        default: return String(format: "0x%02X", device_class)
        }
    }
}

/// An imported (attached) device.
public struct ImportedDeviceInfo: Codable, Identifiable {
    public let port: UInt32
    public let bus_id: String
    public let host: String
    public let vendor_id: UInt16?
    public let product_id: UInt16?

    public var id: UInt32 { port }

    public var vidPid: String? {
        guard let vid = vendor_id, let pid = product_id else { return nil }
        return String(format: "%04x:%04x", vid, pid)
    }
}

/// Manages the Extender daemon process and provides a high-level API.
@MainActor
public final class DaemonManager: ObservableObject {
    @Published public var isRunning = false
    @Published public var localDevices: [DeviceInfo] = []
    @Published public var exportedDevices: [ExportedDeviceInfo] = []
    @Published public var status: DaemonStatus?
    @Published public var lastError: String?
    @Published public var remoteDevices: [RemoteDeviceInfo] = []
    @Published public var importedDevices: [ImportedDeviceInfo] = []
    @Published public var connectedServer: RemoteServer?

    private var daemonProcess: Process?
    private let client = DaemonClient()
    private var pollTimer: Timer?

    public init() {}

    // MARK: - Daemon Lifecycle

    /// Start the daemon as a subprocess.
    public func startDaemon() {
        guard daemonProcess == nil else { return }

        let daemonPath = findDaemonBinary()
        guard let path = daemonPath else {
            lastError = "Cannot find extender binary"
            return
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = ["daemon"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        process.terminationHandler = { [weak self] proc in
            Task { @MainActor in
                self?.isRunning = false
                self?.daemonProcess = nil
                if proc.terminationStatus != 0 {
                    self?.lastError = "Daemon exited with code \(proc.terminationStatus)"
                }
            }
        }

        do {
            try process.run()
            daemonProcess = process
            isRunning = true
            lastError = nil

            // Wait a moment for the daemon to bind its socket, then start polling.
            Task {
                try? await Task.sleep(for: .seconds(1))
                await refresh()
                startPolling()
            }
        } catch {
            lastError = "Failed to start daemon: \(error.localizedDescription)"
        }
    }

    /// Stop the daemon subprocess.
    public func stopDaemon() {
        stopPolling()
        daemonProcess?.terminate()
        daemonProcess = nil
        isRunning = false
        localDevices = []
        exportedDevices = []
        remoteDevices = []
        importedDevices = []
        connectedServer = nil
        status = nil
    }

    /// Try to connect to an already-running daemon.
    public func connectToExisting() async {
        do {
            let s: DaemonStatus = try await client.call(method: "get_status")
            status = s
            isRunning = true
            await refresh()
            startPolling()
        } catch {
            isRunning = false
        }
    }

    // MARK: - Device Operations

    /// Refresh all data from the daemon.
    public func refresh() async {
        do {
            localDevices = try await client.call(method: "list_local_devices")
            exportedDevices = try await client.call(method: "list_exported_devices")
            status = try await client.call(method: "get_status")
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Bind (export) a device.
    public func bindDevice(busId: String) async {
        do {
            let _: [String: AnyCodable] = try await client.call(
                method: "bind_device",
                params: ["bus_id": busId]
            )
            await refresh()
        } catch {
            lastError = "Bind failed: \(error.localizedDescription)"
        }
    }

    /// Unbind (unexport) a device.
    public func unbindDevice(busId: String) async {
        do {
            let _: [String: AnyCodable] = try await client.call(
                method: "unbind_device",
                params: ["bus_id": busId]
            )
            await refresh()
        } catch {
            lastError = "Unbind failed: \(error.localizedDescription)"
        }
    }

    // MARK: - Import Operations

    /// List devices available on a remote server.
    public func listRemoteDevices(host: String, port: UInt16) async {
        do {
            remoteDevices = try await client.call(
                method: "list_remote_devices",
                params: ["host": host, "port": Int(port)]
            )
            connectedServer = RemoteServer(
                id: "\(host):\(port)",
                host: host,
                port: port,
                name: nil
            )
            lastError = nil
        } catch {
            lastError = "Failed to list remote devices: \(error.localizedDescription)"
            remoteDevices = []
            connectedServer = nil
        }
    }

    /// Attach (import) a remote device.
    public func attachDevice(host: String, port: UInt16, busId: String) async {
        do {
            let _: [String: AnyCodable] = try await client.call(
                method: "attach_device",
                params: ["host": host, "port": Int(port), "bus_id": busId]
            )
            // Refresh remote device list and imported devices
            await listRemoteDevices(host: host, port: port)
            await refresh()
        } catch {
            lastError = "Attach failed: \(error.localizedDescription)"
        }
    }

    /// Detach (remove) an imported device.
    public func detachDevice(port: UInt32) async {
        do {
            let _: [String: AnyCodable] = try await client.call(
                method: "detach_device",
                params: ["port": Int(port)]
            )
            await refresh()
        } catch {
            lastError = "Detach failed: \(error.localizedDescription)"
        }
    }

    /// Disconnect from the current remote server.
    public func disconnectServer() {
        connectedServer = nil
        remoteDevices = []
    }

    // MARK: - Polling

    private func startPolling() {
        pollTimer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                await self?.refresh()
            }
        }
    }

    private func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    // MARK: - Helpers

    private func findDaemonBinary() -> String? {
        // Check common locations
        let candidates = [
            // Built from source (release)
            Bundle.main.bundlePath + "/../../../extender/target/release/extender",
            // Development build
            Bundle.main.bundlePath + "/../../../extender/target/debug/extender",
            // Installed
            "/usr/local/bin/extender",
            "/opt/homebrew/bin/extender",
            // Relative to this binary
            Bundle.main.bundlePath + "/Contents/Resources/extender",
        ]

        for path in candidates {
            let resolved = (path as NSString).expandingTildeInPath
            if FileManager.default.isExecutableFile(atPath: resolved) {
                return resolved
            }
        }
        return nil
    }
}
