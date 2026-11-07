import Foundation
import ScreenCaptureKit

enum Command: Equatable {
    case help
    case listDisplays
    case listWindows
    case captureOnce(displayID: UInt32, outputPath: String)
}

/// Strict, dependency-free argument parser. Unknown flags, duplicate flags and
/// positional noise are rejected so a malformed capture request can never be
/// "repaired" into a different capture.
enum CLI {
    static let usageText = """
    ukk-remote-screen-helper — bounded macOS ScreenCaptureKit capture spike

    Usage:
      ukk-remote-screen-helper list-displays
      ukk-remote-screen-helper list-windows
      ukk-remote-screen-helper capture-once --display <id> --out <file.png>
      ukk-remote-screen-helper --help

    list-displays and list-windows print metadata as JSON. capture-once captures
    exactly one frame and writes exactly one PNG. It never captures audio,
    injects input, records continuously or retains frames after the write.

    capture-once requires macOS 14 or newer and a Screen Recording grant for the
    process identity that runs this helper. Without that grant it exits 4.

    Exit codes:
      0 success
      2 usage or invalid output path
      3 unsupported macOS version
      4 Screen Recording permission denied
      5 ScreenCaptureKit capture failed
      6 PNG output could not be written
    """

    static func parse(_ arguments: [String]) throws -> Command {
        guard let first = arguments.first else {
            throw HelperError.usage("missing command")
        }

        switch first {
        case "-h", "--help", "help":
            guard arguments.count == 1 else {
                throw HelperError.usage("--help takes no arguments")
            }
            return .help
        case "list-displays":
            guard arguments.count == 1 else {
                throw HelperError.usage("list-displays takes no arguments")
            }
            return .listDisplays
        case "list-windows":
            guard arguments.count == 1 else {
                throw HelperError.usage("list-windows takes no arguments")
            }
            return .listWindows
        case "capture-once":
            return try parseCaptureOnce(Array(arguments.dropFirst()))
        default:
            throw HelperError.usage("unknown command `\(first)`")
        }
    }

    private static func parseCaptureOnce(_ arguments: [String]) throws -> Command {
        var displayID: UInt32?
        var outputPath: String?
        var index = 0

        while index < arguments.count {
            let argument = arguments[index]

            if argument == "--display" || argument.hasPrefix("--display=") {
                guard displayID == nil else {
                    throw HelperError.usage("duplicate --display")
                }
                let value = try value(
                    for: "--display",
                    inline: argument,
                    arguments: arguments,
                    index: &index
                )
                displayID = try parseDisplayID(value)
            } else if argument == "--out" || argument.hasPrefix("--out=") {
                guard outputPath == nil else {
                    throw HelperError.usage("duplicate --out")
                }
                let value = try value(
                    for: "--out",
                    inline: argument,
                    arguments: arguments,
                    index: &index
                )
                guard !value.isEmpty else {
                    throw HelperError.usage("--out requires a non-empty path")
                }
                outputPath = value
            } else {
                throw HelperError.usage("unexpected argument `\(argument)`")
            }

            index += 1
        }

        guard let displayID else {
            throw HelperError.usage("capture-once requires --display <id>")
        }
        guard let outputPath else {
            throw HelperError.usage("capture-once requires --out <file.png>")
        }
        return .captureOnce(displayID: displayID, outputPath: outputPath)
    }

    /// Accept both `--flag value` and `--flag=value`. The latter is common in
    /// scripts and avoids consuming the next token as a value.
    private static func value(
        for flag: String,
        inline: String,
        arguments: [String],
        index: inout Int
    ) throws -> String {
        if inline.hasPrefix(flag + "=") {
            return String(inline.dropFirst(flag.count + 1))
        }
        let valueIndex = index + 1
        guard valueIndex < arguments.count else {
            throw HelperError.usage("\(flag) requires a value")
        }
        index = valueIndex
        return arguments[valueIndex]
    }

    private static func parseDisplayID(_ text: String) throws -> UInt32 {
        guard !text.isEmpty else {
            throw HelperError.usage("--display requires a numeric display id")
        }
        guard let value = UInt32(text), value > 0 else {
            throw HelperError.usage(
                "invalid display id `\(text)`; expected a positive decimal UInt32 from list-displays"
            )
        }
        return value
    }
}
