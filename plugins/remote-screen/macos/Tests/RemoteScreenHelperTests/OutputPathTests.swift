import Darwin
import Foundation
import XCTest
@testable import RemoteScreenHelper

final class OutputPathTests: XCTestCase {
    func testAcceptsRegularPNGPathsInExistingDirectories() throws {
        let directory = try makeTemporaryDirectory()

        let lower = directory.appendingPathComponent("frame.png")
        XCTAssertEqual(try OutputPath.validate(lower.path).path, lower.path)

        let upper = directory.appendingPathComponent("FRAME.PNG")
        XCTAssertEqual(try OutputPath.validate(upper.path).path, upper.path)

        let existing = directory.appendingPathComponent("existing.png")
        try Data("old".utf8).write(to: existing)
        XCTAssertEqual(try OutputPath.validate(existing.path).path, existing.path)
    }

    func testNormalizesParentTraversalButKeepsTheParentDirectory() throws {
        let directory = try makeTemporaryDirectory()
        try FileManager.default.createDirectory(
            at: directory.appendingPathComponent("sub"),
            withIntermediateDirectories: false
        )

        let expected = directory.appendingPathComponent("frame.png")
        let raw = directory.appendingPathComponent("sub/../frame.png").path
        XCTAssertEqual(try OutputPath.validate(raw).path, expected.path)
    }

    func testRejectsEmptyOversizedNULAndWrongExtensionPaths() {
        assertUsage("")
        assertUsage(String(repeating: "a", count: OutputPath.maxUTF8Bytes) + ".png")
        assertUsage("/tmp/frame\0.png")
        assertUsage("/tmp/frame.jpg")
        assertUsage("/tmp/frame")
        assertUsage("/tmp/frame.png.txt")
    }

    func testRejectsMissingAndNonDirectoryParents() throws {
        let directory = try makeTemporaryDirectory()
        assertUsage(directory.appendingPathComponent("missing/frame.png").path)

        let file = directory.appendingPathComponent("not-a-directory")
        try Data("x".utf8).write(to: file)
        assertUsage(file.appendingPathComponent("frame.png").path)
    }

    func testRejectsExistingDirectorySymlinkAndSpecialFileTargets() throws {
        let directory = try makeTemporaryDirectory()

        let targetDirectory = directory.appendingPathComponent("directory.png")
        try FileManager.default.createDirectory(
            at: targetDirectory,
            withIntermediateDirectories: false
        )
        assertUsage(targetDirectory.path)

        let regularTarget = directory.appendingPathComponent("regular.png")
        try Data("x".utf8).write(to: regularTarget)
        let symlink = directory.appendingPathComponent("symlink.png")
        try FileManager.default.createSymbolicLink(at: symlink, withDestinationURL: regularTarget)
        assertUsage(symlink.path)

        let fifo = directory.appendingPathComponent("fifo.png")
        XCTAssertEqual(mkfifo(fifo.path, 0o600), 0)
        assertUsage(fifo.path)
    }

    func testWritePNGProducesExactlyOneCompletePNG() throws {
        let directory = try makeTemporaryDirectory()
        let output = directory.appendingPathComponent("written.png")

        let image = try XCTUnwrap(makeSolidImage(width: 4, height: 4))
        try OutputPath.writePNG(image, to: output)

        let data = try Data(contentsOf: output)
        XCTAssertGreaterThan(data.count, 8)
        XCTAssertEqual(Array(data.prefix(8)), [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])

        let leftovers = try FileManager.default.contentsOfDirectory(atPath: directory.path)
        XCTAssertEqual(leftovers, ["written.png"], "no temporary frame file may remain")
    }

    func testWritePNGFailureDoesNotLeaveATemporaryFile() throws {
        let directory = try makeTemporaryDirectory()
        let output = directory.appendingPathComponent("written.png")
        let image = try XCTUnwrap(makeSolidImage(width: 4, height: 4))

        // Make the parent non-writable so the PNG destination cannot be created.
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o500)],
            ofItemAtPath: directory.path
        )
        defer {
            try? FileManager.default.setAttributes(
                [.posixPermissions: NSNumber(value: 0o700)],
                ofItemAtPath: directory.path
            )
        }

        do {
            try OutputPath.writePNG(image, to: output)
            XCTFail("expected output failure in a non-writable directory")
        } catch let error as HelperError {
            XCTAssertEqual(error.status, .outputFailed)
        }

        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o700)],
            ofItemAtPath: directory.path
        )
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: directory.path)
        XCTAssertTrue(leftovers.isEmpty, "failed encode must not leave a temporary file: \(leftovers)")
    }

    private func makeTemporaryDirectory() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("ukk-remote-screen-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        addTeardownBlock {
            try? FileManager.default.removeItem(at: url)
        }
        return url
    }

    private func makeSolidImage(width: Int, height: Int) -> CGImage? {
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        guard let context = CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: colorSpace,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else {
            return nil
        }
        context.setFillColor(CGColor(red: 0.2, green: 0.4, blue: 0.6, alpha: 1.0))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()
    }

    private func assertUsage(
        _ path: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        do {
            _ = try OutputPath.validate(path)
            XCTFail("expected usage error for output path `\(path)`", file: file, line: line)
        } catch let error as HelperError {
            XCTAssertEqual(error.status, .usage, file: file, line: line)
        } catch {
            XCTFail("unexpected error \(error)", file: file, line: line)
        }
    }
}
