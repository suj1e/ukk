import Darwin
import Foundation

struct CaptureResult: Codable, Equatable {
    let display: UInt32
    let output: String
    let bytes: Int
}

@main
struct RemoteScreenHelper {
    static func main() async {
        let arguments = Array(CommandLine.arguments.dropFirst())

        do {
            switch try CLI.parse(arguments) {
            case .help:
                print(CLI.usageText)
            case .listDisplays:
                try JSONOutput.print(try await CaptureService.listDisplays())
            case .listWindows:
                try JSONOutput.print(try await CaptureService.listWindows())
            case let .captureOnce(displayID, outputPath):
                let url = try OutputPath.validate(outputPath)
                try await CaptureService.captureOnce(displayID: displayID, to: url)
                let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
                let bytes = (attributes?[.size] as? NSNumber)?.intValue ?? 0
                try JSONOutput.print(
                    CaptureResult(display: displayID, output: url.path, bytes: bytes)
                )
            }
        } catch let error as HelperError {
            FileHandle.standardError.write(Data((error.message + "\n").utf8))
            exit(error.status.rawValue)
        } catch {
            FileHandle.standardError.write(
                Data(("unexpected failure: \(error.localizedDescription)\n").utf8)
            )
            exit(ExitStatus.captureFailed.rawValue)
        }
    }
}
