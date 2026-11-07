import SwiftUI
import XCTest
@testable import UkkCompanion

final class AppModelPairingTests: XCTestCase {
    private let secret = String(repeating: "s", count: 24)

    func testScannedPayloadNeverEntersVisibleDraft() async throws {
        let payload = makePayload(secret: secret)
        let outcome = await MainActor.run { () -> (draft: String, message: String?) in
            let model = AppModel()
            model.handleScannedCode(payload, now: FixtureTime.base)
            return (model.pairingPayloadDraft, model.pairingParseMessage)
        }

        XCTAssertEqual(outcome.draft, "")
        XCTAssertFalse(outcome.draft.contains(secret))

        let message = try XCTUnwrap(outcome.message)
        XCTAssertTrue(message.contains("Invite parsed for host"))
        XCTAssertFalse(message.contains(secret))
    }

    func testValidatePairingDoesNotCopyPayloadIntoDraft() async {
        let payload = makePayload(secret: secret)
        let draft = await MainActor.run { () -> String in
            let model = AppModel()
            model.validatePairing(payload: payload, now: FixtureTime.base)
            return model.pairingPayloadDraft
        }

        XCTAssertEqual(draft, "")
        XCTAssertFalse(draft.contains(secret))
    }

    func testInvalidScanLeavesUserDraftAndParseMessageIntact() async {
        let outcome = await MainActor.run { () -> (draft: String, message: String?) in
            let model = AppModel()
            model.pairingPayloadDraft = "user-authored draft"
            model.handleScannedCode("not an invite")
            return (model.pairingPayloadDraft, model.pairingParseMessage)
        }

        XCTAssertEqual(outcome.draft, "user-authored draft")
        XCTAssertEqual(outcome.message, PairingInviteParseError.malformedJSON.message)
    }

    func testManualDraftValidationStillValidatesTheVisibleDraft() async throws {
        let payload = makePayload(secret: secret)
        let outcome = await MainActor.run { () -> (draft: String, message: String?) in
            let model = AppModel()
            model.pairingPayloadDraft = payload
            model.validatePairingDraft(now: FixtureTime.base)
            return (model.pairingPayloadDraft, model.pairingParseMessage)
        }

        XCTAssertEqual(outcome.draft, payload)
        let message = try XCTUnwrap(outcome.message)
        XCTAssertFalse(message.contains(secret))
    }

    func testScenePhaseKeepsFixtureModeAcrossBackgrounding() async {
        let outcome = await MainActor.run {
            () -> (background: ConnectionState, foreground: ConnectionState, isLive: Bool, retry: Bool) in
            let model = AppModel()
            model.handleScenePhase(.background)
            let background = model.connection.state
            model.handleScenePhase(.active)
            return (
                background,
                model.connection.state,
                model.connection.indicator.isLive,
                model.connection.isRetryAvailable
            )
        }

        XCTAssertEqual(outcome.background, .fixtureData)
        XCTAssertEqual(outcome.foreground, .fixtureData)
        XCTAssertFalse(outcome.isLive)
        XCTAssertFalse(outcome.retry)
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
