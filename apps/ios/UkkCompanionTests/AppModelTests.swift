import XCTest
@testable import UkkCompanion

@MainActor
final class AppModelTests: XCTestCase {
    func testNoPinnedHostMeansHonestFixtureMode() {
        let model = AppModel(vault: IdentityVault(store: InMemorySecretStore()))
        XCTAssertTrue(model.isFixtureMode)
        XCTAssertEqual(model.connection.state, .fixtureData)
        XCTAssertFalse(model.connection.isLiveClaim)
        XCTAssertEqual(model.workspaces, FixtureCatalog.fixture.workspaces)
        XCTAssertTrue(model.devices.isEmpty)
        XCTAssertEqual(model.connection.indicator.label, "Fixture data · no Host")
    }

    func testForgettingHostReturnsToFixtureMode() {
        let model = AppModel(vault: IdentityVault(store: InMemorySecretStore()))
        model.forgetHost()
        XCTAssertTrue(model.isFixtureMode)
        XCTAssertEqual(model.connection.state, .fixtureData)
        XCTAssertTrue(model.navigation.path.isEmpty)
    }

    func testMalformedInviteFailsPairingWithoutNetwork() async {
        let coordinator = PairingCoordinator(vault: IdentityVault(store: InMemorySecretStore()))
        coordinator.begin(payload: "not an invite", now: FixtureTime.base)

        guard await waitUntil({ coordinator.state == .failed(PairingInviteParseError.malformedJSON.message) }) else {
            return XCTFail("pairing did not fail: \(coordinator.state)")
        }
        XCTAssertEqual(coordinator.state, .failed(PairingInviteParseError.malformedJSON.message))
    }

    func testExpiredInviteFailsBeforeAnyConnection() async {
        let secret = Data(repeating: 0x42, count: 32)
        let payload = """
        {"v":1,"host_id":"h","host_ed":"\(Base64URL.encode(Data(repeating: 1, count: 32)))","host_x":"\(Base64URL.encode(Data(repeating: 2, count: 32)))","invite_id":"invite-123456","secret":"\(Base64URL.encode(secret))","expire_ms":1,"relay":"wss://relay.example.invalid/ws"}
        """
        let coordinator = PairingCoordinator(vault: IdentityVault(store: InMemorySecretStore()))
        coordinator.begin(payload: payload, now: FixtureTime.base)

        guard await waitUntil({ coordinator.state == .failed(PairingInviteParseError.expired.message) }) else {
            return XCTFail("pairing did not fail: \(coordinator.state)")
        }
    }

    func testEmptyPairingPayloadIsRejectedLocally() {
        let model = AppModel(vault: IdentityVault(store: InMemorySecretStore()))
        model.beginPairing(payload: "   ")
        XCTAssertEqual(model.lastError, "Paste or scan a pairing invite first.")
        XCTAssertEqual(model.pairing.state, .idle)
    }

    func testFixtureSelectionsAreNotPersisted() throws {
        let suiteName = "dev.ukk.companion.tests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let model = AppModel(
            vault: IdentityVault(store: InMemorySecretStore()),
            defaults: defaults
        )
        let session = try XCTUnwrap(FixtureCatalog.fixture.session(.fixture(201)))
        model.noteSessionOpened(session)

        // Only a Host-backed Session has an RPC id worth restoring.
        XCTAssertNil(defaults.string(forKey: "dev.ukk.companion.last-session"))
        XCTAssertTrue(defaults.dictionaryRepresentation().keys.filter {
            $0.hasPrefix("dev.ukk.companion")
        }.isEmpty)
    }

    private func waitUntil(
        _ condition: @escaping @MainActor () -> Bool,
        attempts: Int = 200
    ) async -> Bool {
        for _ in 0..<attempts {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        return condition()
    }
}
