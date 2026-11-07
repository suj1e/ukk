import XCTest
@testable import UkkCompanion

final class PushHintContractTests: XCTestCase {
    func testParsesAllowlistedOpaqueHint() {
        let hint = PushHintContract.parse(userInfo: ["ukk_hint_id": "hint-42"])
        XCTAssertEqual(hint, OpaquePushHint(hintIdentifier: "hint-42"))
    }

    func testIgnoresLeakingPayloadFields() {
        let userInfo: [AnyHashable: Any] = [
            "ukk_hint_id": "hint-42",
            "title": "Agent needs approval",
            "body": "Sensitive file name",
            "session": "Fix flaky mock protocol",
            "code": "123456"
        ]
        let hint = PushHintContract.parse(userInfo: userInfo)
        XCTAssertEqual(hint?.hintIdentifier, "hint-42")

        let sanitized = PushHintContract.sanitized(userInfo: userInfo)
        XCTAssertEqual(sanitized, ["ukk_hint_id": "hint-42"])
        XCTAssertNil(sanitized["title"])
        XCTAssertNil(sanitized["body"])
        XCTAssertNil(sanitized["code"])
    }

    func testRejectsMissingOrBlankHint() {
        XCTAssertNil(PushHintContract.parse(userInfo: [:]))
        XCTAssertNil(PushHintContract.parse(userInfo: ["ukk_hint_id": "   "]))
        XCTAssertNil(PushHintContract.parse(userInfo: ["ukk_hint_id": 42]))
        XCTAssertTrue(PushHintContract.sanitized(userInfo: [:]).isEmpty)
    }

    func testUnknownPayloadYieldsEmptySanitizedPayload() {
        let sanitized = PushHintContract.sanitized(userInfo: ["body": "secret"])
        XCTAssertTrue(sanitized.isEmpty)
    }
}
