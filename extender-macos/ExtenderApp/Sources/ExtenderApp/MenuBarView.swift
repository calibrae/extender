import SwiftUI
import ExtenderBridge

struct MenuBarView: View {
    @ObservedObject var daemon: DaemonManager

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header
            header

            Divider()

            if daemon.isRunning {
                // Device list
                deviceList

                Divider()

                // Status
                statusSection
            } else {
                notRunningView
            }

            Divider()

            // Footer
            footer
        }
        .frame(width: 360)
        .task {
            await daemon.connectToExisting()
        }
    }

    // MARK: - Header

    private var header: some View {
        HStack {
            Image(systemName: "cable.connector.horizontal")
                .foregroundStyle(.blue)
            Text("Extender")
                .font(.headline)
            Spacer()
            Circle()
                .fill(daemon.isRunning ? .green : .red)
                .frame(width: 8, height: 8)
            Text(daemon.isRunning ? "Running" : "Stopped")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    // MARK: - Device List

    private var deviceList: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("USB Devices")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.top, 6)
                .padding(.bottom, 4)

            if daemon.localDevices.isEmpty {
                Text("No devices found")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 8)
            } else {
                ForEach(daemon.localDevices) { device in
                    DeviceRow(device: device, daemon: daemon)
                }
            }

            if !daemon.exportedDevices.isEmpty {
                Text("Exported")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 12)
                    .padding(.top, 6)
                    .padding(.bottom, 4)

                ForEach(daemon.exportedDevices) { device in
                    ExportedDeviceRow(device: device, daemon: daemon)
                }
            }
        }
    }

    // MARK: - Status

    private var statusSection: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let status = daemon.status {
                HStack {
                    Label("Uptime: \(status.uptimeDisplay)", systemImage: "clock")
                    Spacer()
                    Text("v\(status.version)")
                }
                .font(.caption)
                .foregroundStyle(.secondary)

                if status.exported_devices > 0 || status.active_connections > 0 {
                    HStack {
                        Text("\(status.exported_devices) exported")
                        Text("\(status.active_connections) connected")
                    }
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                }
            }

            if let error = daemon.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    // MARK: - Not Running

    private var notRunningView: some View {
        VStack(spacing: 8) {
            Text("Daemon is not running")
                .font(.caption)
                .foregroundStyle(.secondary)

            Button("Start Daemon") {
                daemon.startDaemon()
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)

            if let error = daemon.lastError {
                Text(error)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .lineLimit(2)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity)
    }

    // MARK: - Footer

    private var footer: some View {
        HStack {
            Button {
                Task { await daemon.refresh() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .buttonStyle(.borderless)

            Spacer()

            if daemon.isRunning {
                Button("Stop") {
                    daemon.stopDaemon()
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.red)
            }

            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
            .buttonStyle(.borderless)
        }
        .font(.caption)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}

// MARK: - Device Row

struct DeviceRow: View {
    let device: DeviceInfo
    @ObservedObject var daemon: DaemonManager

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 4) {
                    Text(device.displayName)
                        .font(.system(.body, design: .default))
                        .lineLimit(1)
                    if device.is_bound {
                        Text("SHARED")
                            .font(.system(size: 9, weight: .medium))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 1)
                            .background(.blue, in: Capsule())
                    }
                }

                HStack(spacing: 8) {
                    Text(device.vidPid)
                    Text(device.bus_id)
                    Text(device.speedDisplay)
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }

            Spacer()

            Button(device.is_bound ? "Unbind" : "Bind") {
                Task {
                    if device.is_bound {
                        await daemon.unbindDevice(busId: device.bus_id)
                    } else {
                        await daemon.bindDevice(busId: device.bus_id)
                    }
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .tint(device.is_bound ? .red : .blue)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
    }
}

// MARK: - Exported Device Row

struct ExportedDeviceRow: View {
    let device: ExportedDeviceInfo
    @ObservedObject var daemon: DaemonManager

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 1) {
                Text(device.product ?? device.bus_id)
                    .font(.body)
                HStack(spacing: 8) {
                    Text(device.bus_id)
                    Text("\(device.num_clients) client(s)")
                }
                .font(.caption2)
                .foregroundStyle(.secondary)
            }

            Spacer()

            Button("Unbind") {
                Task {
                    await daemon.unbindDevice(busId: device.bus_id)
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .tint(.red)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
    }
}
