import AVFoundation
import XCTest
@testable import UkkCompanion

final class CameraAvailabilityTests: XCTestCase {
    func testSimulatorHasNoCameraAndSaysSo() throws {
        #if targetEnvironment(simulator)
        let availability = CameraAvailability.current()
        XCTAssertEqual(availability, .unavailable(.simulator))
        XCTAssertFalse(availability.isAvailable)
        let reason = try XCTUnwrap(availability.unavailableReason)
        XCTAssertTrue(reason.message.contains("Simulator"))
        #else
        throw XCTSkip("Camera availability expectation is simulator-specific")
        #endif
    }

    func testDeniedAndRestrictedAuthorizationAreReportedAsPermissionDenied() {
        XCTAssertEqual(
            CameraAvailability.availability(forAuthorizationStatus: .denied, deviceAvailable: true),
            .unavailable(.permissionDenied)
        )
        XCTAssertEqual(
            CameraAvailability.availability(forAuthorizationStatus: .restricted, deviceAvailable: true),
            .unavailable(.permissionDenied)
        )
        XCTAssertEqual(
            CameraAvailability.availability(forAuthorizationStatus: .denied, deviceAvailable: false),
            .unavailable(.permissionDenied)
        )
    }

    func testUndeterminedAuthorizationRequestsPermission() {
        XCTAssertEqual(
            CameraAvailability.availability(forAuthorizationStatus: .notDetermined, deviceAvailable: true),
            .requestingPermission
        )
        XCTAssertTrue(
            CameraAvailability.availability(forAuthorizationStatus: .notDetermined, deviceAvailable: false)
                .isRequestingPermission
        )
    }

    func testAuthorizedStateDependsOnCaptureDevicePresence() {
        XCTAssertEqual(
            CameraAvailability.availability(forAuthorizationStatus: .authorized, deviceAvailable: true),
            .available
        )
        XCTAssertEqual(
            CameraAvailability.availability(forAuthorizationStatus: .authorized, deviceAvailable: false),
            .unavailable(.noCaptureDevice)
        )
    }

    func testEveryUnavailableReasonHasActionableCopy() {
        let reasons: [CameraAvailability.UnavailableReason] = [
            .simulator,
            .noCaptureDevice,
            .permissionDenied
        ]
        for reason in reasons {
            XCTAssertFalse(reason.message.isEmpty)
        }
        XCTAssertTrue(CameraAvailability.UnavailableReason.permissionDenied.message.contains("Settings"))
    }
}
