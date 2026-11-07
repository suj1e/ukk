import XCTest
@testable import UkkCompanion

final class TrustedDeviceStoreTests: XCTestCase {
    func testFixtureStoreExposesCurrentDeviceLast() {
        let store = TrustedDeviceStore.fixture
        XCTAssertEqual(store.currentDevice?.id, .fixture(501))
        XCTAssertEqual(store.devices.count, 3)
    }

    func testDisplayOrderPutsCurrentFirstAndRevokedLast() {
        let store = TrustedDeviceStore.fixture
        let ordered = store.sortedForDisplay
        XCTAssertEqual(ordered.first?.id, .fixture(501))
        XCTAssertEqual(ordered.last?.id, .fixture(503))
        XCTAssertEqual(ordered.last?.presence, .revoked)
    }

    func testRevokingDeviceClearsGrantsAndPresence() {
        var store = TrustedDeviceStore.fixture
        let outcome = store.revoke(.fixture(502))
        XCTAssertEqual(outcome, .revoked)

        guard let device = store.devices.first(where: { $0.id == .fixture(502) }) else {
            return XCTFail("fixture device missing")
        }
        XCTAssertEqual(device.presence, .revoked)
        XCTAssertTrue(device.scopes.isEmpty)
        XCTAssertEqual(device.scopesSummary, "No grants")
        XCTAssertTrue(outcome.message.contains("No Host request was sent"))
    }

    func testRevokingTwiceReportsAlreadyRevoked() {
        var store = TrustedDeviceStore.fixture
        XCTAssertEqual(store.revoke(.fixture(502)), .revoked)
        XCTAssertEqual(store.revoke(.fixture(502)), .alreadyRevoked)
    }

    func testCurrentDeviceCannotRevokeItself() {
        var store = TrustedDeviceStore.fixture
        XCTAssertEqual(store.revoke(.fixture(501)), .refusedCurrentDevice)
        XCTAssertNotEqual(store.currentDevice?.presence, .revoked)
        XCTAssertEqual(store.currentDevice?.scopes, [.observe])
    }

    func testUnknownDeviceReturnsNotFound() {
        var store = TrustedDeviceStore.fixture
        XCTAssertEqual(store.revoke(.fixture(999)), .notFound)
    }

    func testFixtureScopesAreLeastPrivilege() {
        let store = TrustedDeviceStore.fixture
        let current = store.currentDevice
        XCTAssertEqual(current?.role, .observer)
        XCTAssertEqual(current?.scopes, [.observe])
        XCTAssertFalse(current?.scopes.contains(.agentApprove) ?? true)
        XCTAssertFalse(current?.scopes.contains(.screenControl) ?? true)

        let mac = store.devices.first { $0.id == .fixture(502) }
        XCTAssertEqual(mac?.scopes, [.observe, .agentSend])
        XCTAssertFalse(mac?.scopes.contains(.screenControl) ?? true)
    }

    func testScopeSummaryUsesProtocolOrder() {
        let device = TrustedDevice(
            id: .fixture(900),
            label: "Ordered",
            platform: "test",
            role: .operator,
            scopes: [.filesystemTransfer, .observe, .agentSend],
            presence: .online,
            lastSeenAt: nil,
            isCurrentDevice: false,
            provenance: .fixture
        )
        XCTAssertEqual(device.scopesSummary, "Observe sessions · Send to Agent · Transfer files")
    }
}
