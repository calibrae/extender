import Foundation
import SystemExtensions

@MainActor
public final class SystemExtensionManager: NSObject, ObservableObject, OSSystemExtensionRequestDelegate {
    @Published public var extensionStatus: ExtensionStatus = .unknown
    @Published public var statusMessage: String?

    public enum ExtensionStatus {
        case unknown
        case activating
        case activated
        case needsApproval  // User needs to approve in System Settings
        case failed(String)
    }

    public func activateExtension() {
        let request = OSSystemExtensionRequest.activationRequest(
            forExtensionWithIdentifier: "com.calibrae.extender.driver",
            queue: .main
        )
        request.delegate = self
        OSSystemExtensionManager.shared.submitRequest(request)
        extensionStatus = .activating
        statusMessage = "Activating driver..."
    }

    // MARK: - OSSystemExtensionRequestDelegate

    nonisolated public func request(_ request: OSSystemExtensionRequest, actionForReplacingExtension existing: OSSystemExtensionProperties, withExtension ext: OSSystemExtensionProperties) -> OSSystemExtensionRequest.ReplacementAction {
        return .replace
    }

    nonisolated public func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        Task { @MainActor in
            extensionStatus = .needsApproval
            statusMessage = "Approve the extension in System Settings → Privacy & Security"
        }
    }

    nonisolated public func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        Task { @MainActor in
            switch result {
            case .completed:
                extensionStatus = .activated
                statusMessage = "Driver activated"
            case .willCompleteAfterReboot:
                extensionStatus = .needsApproval
                statusMessage = "Reboot required to complete activation"
            @unknown default:
                extensionStatus = .activated
                statusMessage = nil
            }
        }
    }

    nonisolated public func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        Task { @MainActor in
            extensionStatus = .failed(error.localizedDescription)
            statusMessage = "Driver activation failed: \(error.localizedDescription)"
        }
    }
}
