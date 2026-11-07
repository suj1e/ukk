import Foundation
import ScreenCaptureKit
import XCTest
@testable import RemoteScreenHelper

final class ErrorMappingTests: XCTestCase {
    func testExitStatusValuesAreStable() {
        XCTAssertEqual(ExitStatus.success.rawValue, 0)
        XCTAssertEqual(ExitStatus.usage.rawValue, 2)
        XCTAssertEqual(ExitStatus.unsupportedOS.rawValue, 3)
        XCTAssertEqual(ExitStatus.permissionDenied.rawValue, 4)
        XCTAssertEqual(ExitStatus.captureFailed.rawValue, 5)
        XCTAssertEqual(ExitStatus.outputFailed.rawValue, 6)
    }

    func testDeclinedAndMissingEntitlementMapToPermissionDenied() {
        assertMapsToPermissionDenied(code: SCStreamError.Code.userDeclined.rawValue)
        assertMapsToPermissionDenied(code: SCStreamError.Code.missingEntitlements.rawValue)
    }

    func testOtherScreenCaptureKitErrorsMapToCaptureFailure() {
        let mapped = HelperError.mapCaptureError(
            NSError(
                domain: SCStreamError.errorDomain,
                code: SCStreamError.Code.invalidParameter.rawValue,
                userInfo: [NSLocalizedDescriptionKey: "bad configuration"]
            )
        )
        guard case let .captureFailed(message) = mapped else {
            return XCTFail("expected capture failure, got \(mapped)")
        }
        XCTAssertTrue(message.contains("invalidParameter") || message.contains("bad configuration"))
        XCTAssertEqual(mapped.status, .captureFailed)
    }

    func testUnrelatedErrorsStayCaptureFailures() {
        let mapped = HelperError.mapCaptureError(
            NSError(
                domain: NSCocoaErrorDomain,
                code: NSFileReadUnknownError,
                userInfo: [NSLocalizedDescriptionKey: "synthetic"]
            )
        )
        guard case .captureFailed = mapped else {
            return XCTFail("unexpected mapping \(mapped)")
        }
        XCTAssertEqual(mapped.status, .captureFailed)
    }

    func testEveryErrorCarriesAnExplicitStatus() {
        XCTAssertEqual(HelperError.usage("x").status, .usage)
        XCTAssertEqual(HelperError.unsupportedOS("x").status, .unsupportedOS)
        XCTAssertEqual(HelperError.permissionDenied("x").status, .permissionDenied)
        XCTAssertEqual(HelperError.captureFailed("x").status, .captureFailed)
        XCTAssertEqual(HelperError.outputFailed("x").status, .outputFailed)
    }

    private func assertMapsToPermissionDenied(
        code: Int,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let mapped = HelperError.mapCaptureError(
            NSError(
                domain: SCStreamError.errorDomain,
                code: code,
                userInfo: [NSLocalizedDescriptionKey: "declined by TCC"]
            )
        )
        guard case .permissionDenied = mapped else {
            return XCTFail("expected permission denial, got \(mapped)", file: file, line: line)
        }
        XCTAssertEqual(mapped.status, .permissionDenied, file: file, line: line)
        XCTAssertTrue(mapped.message.contains("Screen Recording"), file: file, line: line)
    }
}
