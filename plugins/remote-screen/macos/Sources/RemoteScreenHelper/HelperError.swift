import Foundation
import ScreenCaptureKit

/// Stable process exit states. Scripts and future host adapters rely on these
/// instead of parsing English output.
enum ExitStatus: Int32 {
    case success = 0
    case usage = 2
    case unsupportedOS = 3
    case permissionDenied = 4
    case captureFailed = 5
    case outputFailed = 6
}

enum HelperError: Error, Equatable {
    case usage(String)
    case unsupportedOS(String)
    case permissionDenied(String)
    case captureFailed(String)
    case outputFailed(String)

    var status: ExitStatus {
        switch self {
        case .usage:
            return .usage
        case .unsupportedOS:
            return .unsupportedOS
        case .permissionDenied:
            return .permissionDenied
        case .captureFailed:
            return .captureFailed
        case .outputFailed:
            return .outputFailed
        }
    }

    var message: String {
        switch self {
        case let .usage(text): return text
        case let .unsupportedOS(text): return text
        case let .permissionDenied(text): return text
        case let .captureFailed(text): return text
        case let .outputFailed(text): return text
        }
    }

    /// Map ScreenCaptureKit errors onto honest process states. Only errors that
    /// the OS actually reports are classified; everything else stays a capture
    /// failure rather than being guessed into a permission problem.
    static func mapCaptureError(_ error: Error) -> HelperError {
        let nsError = error as NSError
        if nsError.domain == SCStreamError.errorDomain,
           let code = SCStreamError.Code(rawValue: nsError.code)
        {
            switch code {
            case .userDeclined:
                return .permissionDenied(
                    "Screen Recording permission was denied. Enable it in System Settings > Privacy & Security > Screen & System Audio Recording for the process that runs this helper, then retry. "
                        + nsError.localizedDescription
                )
            case .missingEntitlements:
                return .permissionDenied(
                    "ScreenCaptureKit reported missing entitlements. A signed helper identity and the Screen Recording grant are required. "
                        + nsError.localizedDescription
                )
            case .noDisplayList, .noCaptureSource:
                return .captureFailed(
                    "ScreenCaptureKit found no capturable display. Run list-displays and check that a display is attached. "
                        + nsError.localizedDescription
                )
            default:
                return .captureFailed(
                    "ScreenCaptureKit error \(nsError.code): \(nsError.localizedDescription)"
                )
            }
        }
        return .captureFailed(
            "\(nsError.domain) error \(nsError.code): \(nsError.localizedDescription)"
        )
    }
}
