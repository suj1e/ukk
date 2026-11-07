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
}
