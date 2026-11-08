import AVFoundation
import Foundation

/// Camera availability is resolved deterministically so simulator and permission
/// failures are explicit rather than silent.
enum CameraAvailability: Equatable, Sendable {
    enum UnavailableReason: Equatable, Sendable {
        case simulator
        case noCaptureDevice
        case permissionDenied

        var message: String {
            switch self {
            case .simulator:
                return "The iOS Simulator has no camera. QR scanning needs a physical iPhone or iPad; paste the invite JSON instead."
            case .noCaptureDevice:
                return "No capture device is available on this device. QR scanning cannot start."
            case .permissionDenied:
                return "Camera access is denied or restricted. Enable camera access for ukk in Settings to scan a pairing QR code, or paste the invite JSON instead."
            }
        }
    }

    case available
    /// The user has not answered the camera permission prompt yet.
    case requestingPermission
    case unavailable(UnavailableReason)

    var isAvailable: Bool {
        if case .available = self { return true }
        return false
    }

    var isRequestingPermission: Bool {
        if case .requestingPermission = self { return true }
        return false
    }

    var unavailableReason: UnavailableReason? {
        if case .unavailable(let reason) = self { return reason }
        return nil
    }

    /// Synchronous snapshot used by the capture controller. On a physical device
    /// an unanswered permission prompt is reported as `.requestingPermission`.
    static func current() -> CameraAvailability {
        #if targetEnvironment(simulator)
        return .unavailable(.simulator)
        #else
        return availability(
            forAuthorizationStatus: AVCaptureDevice.authorizationStatus(for: .video),
            deviceAvailable: AVCaptureDevice.default(for: .video) != nil
        )
        #endif
    }

    /// Resolves authorization, requesting access once when it has never been
    /// asked. Safe to await from the pairing screen's task.
    static func resolve() async -> CameraAvailability {
        #if targetEnvironment(simulator)
        return .unavailable(.simulator)
        #else
        let status = AVCaptureDevice.authorizationStatus(for: .video)
        guard status == .notDetermined else {
            return availability(
                forAuthorizationStatus: status,
                deviceAvailable: AVCaptureDevice.default(for: .video) != nil
            )
        }

        let granted = await withCheckedContinuation { continuation in
            AVCaptureDevice.requestAccess(for: .video) { granted in
                continuation.resume(returning: granted)
            }
        }
        return availability(
            forAuthorizationStatus: granted ? .authorized : .denied,
            deviceAvailable: AVCaptureDevice.default(for: .video) != nil
        )
        #endif
    }

    /// Pure mapping, kept testable without touching real camera permissions.
    static func availability(
        forAuthorizationStatus status: AVAuthorizationStatus,
        deviceAvailable: Bool
    ) -> CameraAvailability {
        switch status {
        case .authorized:
            return deviceAvailable ? .available : .unavailable(.noCaptureDevice)
        case .denied, .restricted:
            return .unavailable(.permissionDenied)
        case .notDetermined:
            return .requestingPermission
        @unknown default:
            return .unavailable(.permissionDenied)
        }
    }
}
