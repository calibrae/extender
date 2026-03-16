import SwiftUI
import ExtenderBridge

@main
struct ExtenderApp: App {
    @StateObject private var daemon = DaemonManager()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView(daemon: daemon)
        } label: {
            Image(systemName: "cable.connector.horizontal")
        }
        .menuBarExtraStyle(.window)
    }
}
