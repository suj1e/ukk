import SwiftUI
import XCTest
@testable import UkkCompanion

@MainActor
final class AppModelPairingTests: XCTestCase {
    private let secret = String(repeating: "s", count: 24)

    func testScannedPayloadNeverEntersVisibleDraft() throws {
        let model = AppModel()
        model.handleScannedCode(makePayload(secret: secret))

        XCTAssertEqual(model.pairingPayloadDraft, "")
        XCTAssertFalse(model.pairingPayloadDraft.contains(secret))

        let message = try XCTUnwrap(model.pairingParseMessage)
        XCTAssertTrue(message.contains("Invite parsed for host"))
        XCTAssertFalse(message.contains(secret))
    }

    func testValidatePairingDoesNotCopyPayloadIntoDraft() {
        let model = AppModel()
        model.validatePairing(payload: makePayload(secret: secret), now: FixtureTime.base)

        XCTAssertEqual(model.pairingPayloadDraft, "")
        XCTAssertFalse(model.pairingPayloadDraft.contains(secret))
    }

    func testInvalidScanLeavesUserDraftAndParseMessageIntact() {
        let model = AppModel()
        model.pairingPayloadDraft = "user-authored draft"

        model.handleScannedCode("not an invite")

        XCTAssertEqual(model.pairingPayloadDraft, "user-authored draft")
        XCTAssertEqual(model.pairingParseMessage, PairingInviteParseError.malformedJSON.message)
    }

    func testManualDraftValidationStillValidatesTheVisibleDraft() throws {
        let payload = makePayload(secret: secret)
        let model = AppModel()
        model.pairingPayloadDraft = payload

        model.validatePairingDraft(now: FixtureTime.base)

        XCTAssertEqual(model.pairingPayloadDraft, payload)
        let message = try XCTUnwrap(model.pairingParseMessage)
        XCTAssertFalse(message.contains(secret))
    }

    func testScenePhaseKeepsFixtureModeAcrossBackgrounding() {
        let model = AppModel()

        model.handleScenePhase(.background)
        XCTAssertEqual(model.connection.state, .fixtureData)
        XCTAssertFalse(model.connection.indicator.isLive)

        model.handleScenePhase(.active)
        XCTAssertEqual(model.connection.state, .fixtureData)
        XCTAssertFalse(model.connection.indicator.isLive)
        XCTAssertFalse(model.connection.isRetryAvailable)
    }

    private func makePayload(secret: String) -> String {
        let object: [String: Any] = [
            "v": 1,
            "relay": "https://relay.example.invalid",
            "host_id": "11111111-2222-4333-8444-555555555555",
            "host_public_key": String(repeating: "A", count: 43),
            "invite_id": "invite-123456",
            "invite_secret": secret,
            "expires_at": iso8601(FixtureTime.minutes(2))
        ]
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
}
