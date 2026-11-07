import XCTest
@testable import RemoteScreenHelper

final class CLITests: XCTestCase {
    func testParsesSupportedCommands() throws {
        XCTAssertEqual(try CLI.parse(["list-displays"]), .listDisplays)
        XCTAssertEqual(try CLI.parse(["list-windows"]), .listWindows)
        XCTAssertEqual(try CLI.parse(["--help"]), .help)
        XCTAssertEqual(try CLI.parse(["-h"]), .help)
        XCTAssertEqual(try CLI.parse(["help"]), .help)

        XCTAssertEqual(
            try CLI.parse(["capture-once", "--display", "1", "--out", "/tmp/frame.png"]),
            .captureOnce(displayID: 1, outputPath: "/tmp/frame.png")
        )
        XCTAssertEqual(
            try CLI.parse(["capture-once", "--out", "/tmp/frame.png", "--display", "42"]),
            .captureOnce(displayID: 42, outputPath: "/tmp/frame.png")
        )
        XCTAssertEqual(
            try CLI.parse(["capture-once", "--display=7", "--out=/tmp/frame.png"]),
            .captureOnce(displayID: 7, outputPath: "/tmp/frame.png")
        )
        XCTAssertEqual(
            try CLI.parse([
                "capture-once", "--display", "4294967295", "--out", "relative.png",
            ]),
            .captureOnce(displayID: UInt32.max, outputPath: "relative.png")
        )
    }

    func testRejectsMissingAndUnknownCommands() {
        assertUsage([])
        assertUsage(["unknown-command"])
        assertUsage(["capture"])
        assertUsage(["list-displays", "extra"])
        assertUsage(["list-windows", "--all"])
        assertUsage(["--help", "extra"])
    }

    func testRejectsMalformedCaptureArguments() {
        assertUsage(["capture-once"])
        assertUsage(["capture-once", "--display", "1"])
        assertUsage(["capture-once", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display"])
        assertUsage(["capture-once", "--out"])
        assertUsage(["capture-once", "--display", "1", "--out"])
        assertUsage(["capture-once", "--display", "1", "--out", ""])
        assertUsage(["capture-once", "--display=1", "--out="])
        assertUsage(["capture-once", "--display", "1", "--out", "/tmp/frame.png", "extra"])
        assertUsage(["capture-once", "--display", "1", "--display", "2", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display", "1", "--out", "/tmp/a.png", "--out", "/tmp/b.png"])
        assertUsage(["capture-once", "--display", "1", "--verbose", "--out", "/tmp/frame.png"])
    }

    func testRejectsInvalidDisplayIDs() {
        assertUsage(["capture-once", "--display", "zero", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display", "-1", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display", "0", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display", "1.5", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display", "4294967296", "--out", "/tmp/frame.png"])
        assertUsage(["capture-once", "--display", "", "--out", "/tmp/frame.png"])
    }

    func testUsageTextStatesCaptureLimitsAndExitCodes() {
        XCTAssertTrue(CLI.usageText.contains("exactly one frame"))
        XCTAssertTrue(CLI.usageText.contains("never captures audio"))
        XCTAssertTrue(CLI.usageText.contains("Screen Recording"))
        XCTAssertTrue(CLI.usageText.contains("4 Screen Recording permission denied"))
        XCTAssertTrue(CLI.usageText.contains("3 unsupported macOS version"))
    }

    private func assertUsage(
        _ arguments: [String],
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        do {
            _ = try CLI.parse(arguments)
            XCTFail("expected usage error for \(arguments)", file: file, line: line)
        } catch let error as HelperError {
            XCTAssertEqual(error.status, .usage, file: file, line: line)
        } catch {
            XCTFail("unexpected error \(error)", file: file, line: line)
        }
    }
}
