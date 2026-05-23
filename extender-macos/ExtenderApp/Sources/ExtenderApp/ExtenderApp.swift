import SwiftUI

@main
struct ExtenderApp: App {
    @StateObject private var daemon = DaemonManager()

    init() {
        // Kick activation immediately on launch so the user doesn't have to open
        // the menu bar popup before the dext install prompt appears.
        let mgr = SystemExtensionManager()
        Task { @MainActor in
            mgr.activateExtension()
        }
    }

    var body: some Scene {
        MenuBarExtra {
            MenuBarView(daemon: daemon)
        } label: {
            Image(systemName: "cable.connector.horizontal")
        }
        .menuBarExtraStyle(.window)
    }
}
