import XCTest
@testable import UkkCompanion

final class KeychainVaultTests: XCTestCase {
    func testIdentityRoundTripsAndIsStable() throws {
        let store = InMemorySecretStore()
        let vault = IdentityVault(store: store)

        let first = try vault.loadOrCreateIdentity()
        let second = try vault.loadOrCreateIdentity()

        XCTAssertEqual(first, second)
        XCTAssertEqual(first.edSeed.count, 32)
        XCTAssertEqual(first.xPrivate.count, 32)
        XCTAssertEqual(first.edPublic.count, 32)
        XCTAssertEqual(first.xPublic.count, 32)
        XCTAssertEqual(first.deviceID, IdentityRecord.makeDeviceID(edPublic: first.edPublic))
        XCTAssertEqual(store.accounts, ["identity.v1"])
    }

    func testTrustRoundTripsAndClears() throws {
        let store = InMemorySecretStore()
        let vault = IdentityVault(store: store)
        let identity = try vault.loadOrCreateIdentity()

        let trust = HostTrustRecord(
            hostID: "11111111-2222-4333-8444-555555555555",
            hostEd: Data(repeating: 0x11, count: 32),
            hostX: Data(repeating: 0x22, count: 32),
            relayURL: URL(string: "wss://relay.example.invalid/ws")!,
            deviceID: identity.deviceID,
            label: "Desktop fixture",
            pairedAtMS: 1_762_550_000_000
        )

        try vault.saveTrust(trust)
        XCTAssertEqual(try vault.loadTrust(), trust)
        XCTAssertEqual(store.accounts, ["host-trust.v1", "identity.v1"])

        XCTAssertNoThrow(try trust.pinnedIdentity)
        try vault.clearTrust()
        XCTAssertNil(try vault.loadTrust())
        XCTAssertEqual(store.accounts, ["identity.v1"])
    }

    func testStoredRecordsContainNoInviteSecretOrChannelKeys() throws {
        let store = InMemorySecretStore()
        let vault = IdentityVault(store: store)
        let identity = try vault.loadOrCreateIdentity()
        try vault.saveTrust(
            HostTrustRecord(
                hostID: "host",
                hostEd: Data(repeating: 1, count: 32),
                hostX: Data(repeating: 2, count: 32),
                relayURL: URL(string: "wss://relay.example.invalid")!,
                deviceID: identity.deviceID,
                label: "Host",
                pairedAtMS: 0
            )
        )

        // The invite secret is 0x42 repeated; it must not appear in either item.
        let secret = Data(repeating: 0x42, count: 32)
        for account in store.accounts {
            let data = try XCTUnwrap(store.read(account: account))
            XCTAssertNil(data.range(of: secret))
        }
        let identityJSON = String(data: try XCTUnwrap(store.read(account: "identity.v1")), encoding: .utf8) ?? ""
        XCTAssertFalse(identityJSON.contains("secret"))
        XCTAssertFalse(identityJSON.contains("c2s"))
    }

    func testMalformedStoredTrustIsRejected() throws {
        let store = InMemorySecretStore()
        try store.write(Data("not json".utf8), account: "host-trust.v1")
        let vault = IdentityVault(store: store)
        XCTAssertThrowsError(try vault.loadTrust()) { error in
            XCTAssertEqual(error as? SecretStoreError, .malformedRecord)
        }
    }

    func testRejectsZeroedIdentityMaterial() {
        XCTAssertThrowsError(try DeviceIdentity(edSeed: Data(count: 32), xPrivate: Data(count: 32))) { error in
            XCTAssertEqual(error as? SecureChannelError, .invalidIdentity)
        }
    }
}
