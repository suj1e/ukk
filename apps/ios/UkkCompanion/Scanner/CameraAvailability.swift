import Foundation
import AVFoundation

/// Camera availability is resolved deterministically so the simulator path is
/// explicit rather than a silent failure. This build never connects to a Host.
enum CameraAvailability: Equatable, Sendable {
    enum UnavailableReason: Equatable, Sendable {
        case simulator
        case noCaptureDevice

        var message: String {
            switch self {
            case .simulator:
                return "The iOS Simulator has no camera. QR scanning needs a physical iPhone or iPad; this fixture build never connects to a Host."
            case .noCaptureDevice:
                return "No capture device is available on this device. QR scanning cannot start."
            }
        }
    }

    case available
    case unavailable(UnavailableReason)

    var isAvailable: Bool {
        if case .available = self { return true }
        return false
    }

    var unavailableReason: UnavailableReason? {
        if case .unavailable(let reason) = self { return reason }
        return nil
    }

    static func current() -> CameraAvailability {
        #if targetEnvironment(simulator)
        return .unavailable(.simulator)
        #else
        guard AVCaptureDevice.default(for: .video) != nil else {
            return .unavailable(.noCaptureDevice)
        }
        return .available
        #endif
    }
}
