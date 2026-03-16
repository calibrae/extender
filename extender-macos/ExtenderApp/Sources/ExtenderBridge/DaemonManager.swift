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

/// Manages the Extender daemon process and provides a high-level API.
@MainActor
public final class DaemonManager: ObservableObject {
    @Published public var isRunning = false
    @Published public var localDevices: [DeviceInfo] = []
    @Published public var exportedDevices: [ExportedDeviceInfo] = []
    @Published public var status: DaemonStatus?
    @Published public var lastError: String?

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
