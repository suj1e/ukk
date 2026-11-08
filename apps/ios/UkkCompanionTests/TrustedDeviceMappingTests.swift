import XCTest
@testable import UkkCompanion

final class TrustedDeviceMappingTests: XCTestCase {
    func testRemoteDeviceProjectsScopesAndRole() {
        let device = TrustedDevice(
            remote: RemoteDevice(
                id: "device-1",
                label: "Studio iPhone",
                scopes: ["session.observe", "agent.send", "screen.view"],
                expiresMS: 1_762_550_000_000,
                revoked: false,
                createdMS: 0
            ),
            currentDeviceID: "device-1"
        )

        XCTAssertTrue(device.isCurrentDevice)
        XCTAssertEqual(device.remoteID, "device-1")
        XCTAssertEqual(device.role, .operator)
        XCTAssertEqual(device.presence, .online)
        XCTAssertTrue(device.scopes.contains(.observe))
        XCTAssertTrue(device.scopes.contains(.agentSend))
        XCTAssertFalse(device.scopes.contains(.agentApprove))
        XCTAssertEqual(device.id, DeviceID.fromHost("device-1"))
    }

    func testObserverWithoutControlIsObserver() {
        let device = TrustedDevice(
            remote: RemoteDevice(
                id: "device-2",
                label: "iPad",
                scopes: ["session.observe"],
                expiresMS: nil,
                revoked: false,
                createdMS: 0
            ),
            currentDeviceID: "device-1"
        )
        XCTAssertEqual(device.role, .observer)
        XCTAssertEqual(device.scopesSummary, "Observe sessions")
        XCTAssertNil(device.grantedUntil)
    }

    func testScopeSummaryUsesProtocolOrderAndRevocationTone() {
        let device = TrustedDevice(
            remote: RemoteDevice(
                id: "device-3",
                label: "Old phone",
                scopes: ["filesystem.transfer", "agent.send", "session.observe"],
                expiresMS: nil,
                revoked: true,
                createdMS: 0
            ),
            currentDeviceID: "device-1"
        )
        XCTAssertEqual(device.presence, .revoked)
        XCTAssertEqual(device.scopesSummary, "Observe sessions · Send to Agent · Transfer files")
        XCTAssertEqual(device.presence.tone, .negative)
    }

    func testHostIdentifiersAreStableAndDistinct() {
        XCTAssertEqual(DeviceID.fromHost("abc"), DeviceID.fromHost("abc"))
        XCTAssertNotEqual(DeviceID.fromHost("abc"), DeviceID.fromHost("abd"))
        XCTAssertEqual(
            SessionID.fromHost("11111111-2222-4333-8444-555555555555"),
            SessionID(rawValue: UUID(uuidString: "11111111-2222-4333-8444-555555555555")!)
        )
    }
}
