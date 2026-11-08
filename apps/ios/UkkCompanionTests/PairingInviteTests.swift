import XCTest
@testable import UkkCompanion

final class PairingInviteTests: XCTestCase {
    private let secret = Data(repeating: 0x42, count: 32)
    private let now = FixtureTime.base

    func testParsesV1InviteAndRedactsSecret() throws {
        let invite = try parse(inviteObject()).get()

        XCTAssertEqual(invite.version, 1)
        XCTAssertEqual(invite.relay.scheme, "wss")
        XCTAssertEqual(invite.relay.host, "relay.example.invalid")
        XCTAssertEqual(invite.hostID, "11111111-2222-4333-8444-555555555555")
        XCTAssertEqual(invite.hostEd, Data(repeating: 0x11, count: 32))
        XCTAssertEqual(invite.hostX, Data(repeating: 0x22, count: 32))
        XCTAssertEqual(invite.inviteID, "invite-123456")
        XCTAssertEqual(invite.secret, secret)
        XCTAssertEqual(invite.expireMS, expireMS)
        XCTAssertFalse(invite.description.contains(Base64URL.encode(secret)))
        XCTAssertFalse(invite.description.lowercased().contains("secret"))
    }

    func testRejectsMalformedJSON() {
        assertFailure("not an invite", .malformedJSON)
        assertFailure("{} trailing", .malformedJSON)
    }

    func testRejectsUnknownFields() {
        var object = inviteObject()
        object["extra"] = "value"
        assertFailure(object, .unknownField("extra"))
    }

    func testRejectsUnsupportedVersion() {
        var object = inviteObject()
        object["v"] = 2
        assertFailure(object, .unsupportedVersion(2))
    }

    func testRejectsMissingField() {
        var object = inviteObject()
        object.removeValue(forKey: "host_x")
        assertFailure(object, .missingField("host_x"))
    }

    func testRejectsNonWSSRelay() {
        var object = inviteObject()
        object["relay"] = "https://relay.example.invalid"
        assertFailure(object, .invalidRelayOrigin)

        object["relay"] = "ws://relay.example.invalid"
        assertFailure(object, .invalidRelayOrigin)
    }

    func testRejectsRelayWithUserInfo() {
        var object = inviteObject()
        object["relay"] = "wss://user@relay.example.invalid/ws"
        assertFailure(object, .invalidRelayOrigin)
    }

    func testRejectsWrongKeyLengths() {
        var object = inviteObject()
        object["host_ed"] = Base64URL.encode(Data(repeating: 1, count: 31))
        assertFailure(object, .invalidKey("host_ed"))

        object = inviteObject()
        object["host_x"] = Base64URL.encode(Data(repeating: 1, count: 33))
        assertFailure(object, .invalidKey("host_x"))
    }

    func testRejectsShortOrMissingSecret() {
        var object = inviteObject()
        object["secret"] = Base64URL.encode(Data(repeating: 1, count: 16))
        assertFailure(object, .invalidInviteSecret)

        object.removeValue(forKey: "secret")
        assertFailure(object, .missingField("secret"))
    }

    func testRejectsExpiredAndOverlongLifetimes() {
        assertFailure(inviteObject(), .expired, now: Date(timeIntervalSince1970: Double(expireMS) / 1000))

        var object = inviteObject()
        object["expire_ms"] = Int64(now.timeIntervalSince1970 * 1000) + 300_001
        assertFailure(object, .lifetimeTooLong)
    }

    func testAcceptsPaddedBase64URL() throws {
        var object = inviteObject()
        object["host_ed"] = paddedBase64(Data(repeating: 0x11, count: 32))
        _ = try parse(object).get()
    }

    func testEveryParseErrorMessageIsActionable() {
        let errors: [PairingInviteParseError] = [
            .malformedJSON,
            .unknownField("x"),
            .unsupportedVersion(2),
            .missingField("relay"),
            .invalidRelayOrigin,
            .invalidHostID,
            .invalidKey("host_ed"),
            .invalidInviteIdentifier,
            .invalidInviteSecret,
            .invalidExpiry,
            .expired,
            .lifetimeTooLong
        ]
        for error in errors {
            XCTAssertFalse(error.message.isEmpty)
        }
    }

    // MARK: - Helpers

    private var expireMS: Int64 {
        Int64(now.timeIntervalSince1970 * 1000) + 120_000
    }

    private func inviteObject() -> [String: Any] {
        [
            "v": 1,
            "host_id": "11111111-2222-4333-8444-555555555555",
            "host_ed": Base64URL.encode(Data(repeating: 0x11, count: 32)),
            "host_x": Base64URL.encode(Data(repeating: 0x22, count: 32)),
            "invite_id": "invite-123456",
            "secret": Base64URL.encode(secret),
            "expire_ms": expireMS,
            "relay": "wss://relay.example.invalid/ws"
        ]
    }

    private func parse(_ object: [String: Any], now: Date? = nil) -> Result<PairingInvite, PairingInviteParseError> {
        let data = try! JSONSerialization.data(withJSONObject: object)
        return PairingInviteParser.parse(payload: String(data: data, encoding: .utf8)!, now: now ?? self.now)
    }

    private func paddedBase64(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
    }

    private func assertFailure(
        _ object: [String: Any],
        _ expected: PairingInviteParseError,
        now: Date? = nil,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        switch parse(object, now: now) {
        case .success(let invite):
            XCTFail("expected \(expected), parsed \(invite)", file: file, line: line)
        case .failure(let error):
            XCTAssertEqual(error, expected, file: file, line: line)
        }
    }

    private func assertFailure(
        _ payload: String,
        _ expected: PairingInviteParseError,
        now: Date? = nil,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        switch PairingInviteParser.parse(payload: payload, now: now ?? self.now) {
        case .success(let invite):
            XCTFail("expected \(expected), parsed \(invite)", file: file, line: line)
        case .failure(let error):
            XCTAssertEqual(error, expected, file: file, line: line)
        }
    }
}
