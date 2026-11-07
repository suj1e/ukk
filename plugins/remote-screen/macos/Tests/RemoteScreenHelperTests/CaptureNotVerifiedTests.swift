import Foundation
import XCTest
@testable import RemoteScreenHelper

/// Real capture requires a Screen Recording grant for the process identity that
/// runs the helper. CI has no such grant for an unsigned SwiftPM executable, so
/// this test is intentionally skipped there and reports NOT VERIFIED. An
/// operator on a manually granted machine can opt in:
///
///     UKK_RUN_SCREEN_CAPTURE_TESTS=1 swift test --filter CaptureNotVerifiedTests
final class CaptureNotVerifiedTests: XCTestCase {
    func testCaptureOnceIsNotVerifiedWithoutScreenRecordingPermission() async throws {
        guard ProcessInfo.processInfo.environment["UKK_RUN_SCREEN_CAPTURE_TESTS"] == "1" else {
            throw XCTSkip(
                "NOT VERIFIED: real capture-once needs macOS Screen Recording permission and a "
                    + "helper identity accepted by TCC. CI skips this test; set "
                    + "UKK_RUN_SCREEN_CAPTURE_TESTS=1 on a manually granted machine to exercise it."
            )
        }

        let displays: [DisplayInfo]
        do {
            displays = try await CaptureService.listDisplays()
        } catch let error as HelperError where error.status == .permissionDenied {
            throw XCTSkip("NOT VERIFIED: Screen Recording permission denied: \(error.message)")
        }

        let display = try XCTUnwrap(displays.first, "no capturable display is attached")

        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("ukk-remote-screen-capture-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock {
            try? FileManager.default.removeItem(at: directory)
        }

        let output = directory.appendingPathComponent("frame.png")
        let validated = try OutputPath.validate(output.path)

        do {
            try await CaptureService.captureOnce(displayID: display.id, to: validated)
        } catch let error as HelperError where error.status == .permissionDenied {
            throw XCTSkip("NOT VERIFIED: Screen Recording permission denied: \(error.message)")
        }

        let data = try Data(contentsOf: validated)
        XCTAssertGreaterThan(data.count, 8)
        XCTAssertEqual(Array(data.prefix(8)), [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])

        let contents = try FileManager.default.contentsOfDirectory(atPath: directory.path)
        XCTAssertEqual(contents, ["frame.png"], "capture-once must emit exactly one requested file")
    }
}
