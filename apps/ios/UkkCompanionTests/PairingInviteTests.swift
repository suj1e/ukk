import XCTest
@testable import UkkCompanion

final class PairingInviteTests: XCTestCase {
    private let secret = String(repeating: "s", count: 24)

    func testParsesDocumentedInviteAndRedactsSecret() throws {
        let invite = try PairingInviteParser.parse(
            payload: serialized(inviteObject()),
            now: FixtureTime.base
        ).get()

        XCTAssertEqual(invite.version, 1)
        XCTAssertEqual(invite.relayOrigin.host, "relay.example.invalid")
        XCTAssertEqual(invite.relayOrigin.scheme, "https")
        XCTAssertEqual(invite.hostID.uuidString, "11111111-2222-4333-8444-555555555555")
        XCTAssertEqual(invite.inviteID, "invite-123456")
        XCTAssertEqual(invite.inviteSecret, secret)
        XCTAssertFalse(invite.description.contains(secret))
        XCTAssertTrue(invite.description.contains("PairingInvite"))
    }

    func testAcceptsWebSocketSecureRelay() throws {
        var object = inviteObject()
        object["relay"] = "wss://relay.example.invalid/route"
        let invite = try PairingInviteParser.parse(payload: serialized(object), now: FixtureTime.base).get()
        XCTAssertEqual(invite.relayOrigin.scheme, "wss")
    }

    func testRejectsMalformedJSON() {
        assertFailure("not an invite", equals: .malformedJSON)
    }

    func testRejectsUnsupportedVersion() {
        var object = inviteObject()
        object["v"] = 2
        assertFailure(serialized(object), equals: .unsupportedVersion(2))
    }

    func testRejectsMissingField() {
        var object = inviteObject()
        object.removeValue(forKey: "host_id")
        assertFailure(serialized(object), equals: .missingField("host_id"))
    }

    func testRejectsInsecureRelayOrigin() {
        var object = inviteObject()
        object["relay"] = "http://relay.example.invalid"
        assertFailure(serialized(object), equals: .invalidRelayOrigin)
    }

    func testRejectsInvalidHostIdentifier() {
        var object = inviteObject()
        object["host_id"] = "not-a-uuid"
        assertFailure(serialized(object), equals: .invalidHostID)
    }

    func testRejectsNonBase64URLPublicKey() {
        var object = inviteObject()
        object["host_public_key"] = "not a base64url key!!!!!!!!!!!!!!!!!!!!!!!!!!"
        assertFailure(serialized(object), equals: .invalidPublicKey)
    }

    func testRejectsShortInviteIdentifier() {
        var object = inviteObject()
        object["invite_id"] = "x"
        assertFailure(serialized(object), equals: .invalidInviteIdentifier)
    }

    func testRejectsShortInviteSecret() {
        var object = inviteObject()
        object["invite_secret"] = "short"
        assertFailure(serialized(object), equals: .invalidInviteSecret)
    }

    func testRejectsInvalidExpiry() {
        var object = inviteObject()
        object["expires_at"] = "tomorrow"
        assertFailure(serialized(object), equals: .invalidExpiry)
    }

    func testRejectsExpiredInvite() {
        assertFailure(
            serialized(inviteObject()),
            equals: .expired,
            now: FixtureTime.minutes(5)
        )
        assertFailure(
            serialized(inviteObject()),
            equals: .expired,
            now: FixtureTime.minutes(2)
        )
    }

    func testEveryParseErrorMessageIsActionable() {
        let errors: [PairingInviteParseError] = [
            .malformedJSON,
            .unsupportedVersion(2),
            .missingField("relay"),
            .invalidRelayOrigin,
            .invalidHostID,
            .invalidPublicKey,
            .invalidInviteIdentifier,
            .invalidInviteSecret,
            .invalidExpiry,
            .expired
        ]
        for error in errors {
            XCTAssertFalse(error.message.isEmpty)
        }
    }

    // MARK: - Helpers

    private func inviteObject() -> [String: Any] {
        [
            "v": 1,
            "relay": "https://relay.example.invalid",
            "host_id": "11111111-2222-4333-8444-555555555555",
            "host_public_key": String(repeating: "A", count: 43),
            "invite_id": "invite-123456",
            "invite_secret": secret,
            "expires_at": iso8601(FixtureTime.minutes(2))
        ]
    }

    private func serialized(_ object: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: object),
              let json = String(data: data, encoding: .utf8) else {
            XCTFail("failed to serialize invite fixture")
            return "{}"
        }
        return json
    }

    private func iso8601(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }

    private func assertFailure(
        _ payload: String,
        equals expected: PairingInviteParseError,
        now: Date = FixtureTime.base,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        switch PairingInviteParser.parse(payload: payload, now: now) {
        case .success(let invite):
            XCTFail("expected \(expected), parsed \(invite)", file: file, line: line)
        case .failure(let error):
            XCTAssertEqual(error, expected, file: file, line: line)
        }
    }
}
