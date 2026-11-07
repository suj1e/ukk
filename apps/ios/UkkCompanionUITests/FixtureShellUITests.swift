import XCTest

/// Black-box smoke test of the fixture-first shell. It asserts the honest
/// states: fixture labelling, no live claim, read-only composer, unsupported
/// approvals, reserved Remote Screen slot and simulator camera handling.
final class FixtureShellUITests: XCTestCase {
    private var app: XCUIApplication!

    private enum FixtureID {
        static let workspace = "0A11CE00-0000-4000-8000-000000000001"
        static let space = "0A11CE00-0000-4000-8000-000000000101"
        static let session = "0A11CE00-0000-4000-8000-000000000201"
        static let firstMessage = "0A11CE00-0000-4000-8000-000000000401"
        static let protocolRepairAgent = "0A11CE00-0000-4000-8000-000000000301"
        static let studioMacDevice = "0A11CE00-0000-4000-8000-000000000502"
    }

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launch()
    }

    func testFixtureBannerAndNeverLiveStatus() {
        let banner = app.staticTexts["fixtureBannerLabel"]
        XCTAssertTrue(banner.waitForExistence(timeout: 30))
        XCTAssertTrue(banner.label.contains("FIXTURE"))

        let status = app.staticTexts["connectionStatusLabel"]
        XCTAssertTrue(status.waitForExistence(timeout: 10))
        XCTAssertTrue(status.label.contains("Fixture data"))
        XCTAssertNotEqual(status.label, "Live")
        XCTAssertFalse(app.staticTexts["Live"].exists)
        XCTAssertTrue(app.staticTexts["notLiveBadge"].exists)
    }

    func testNavigationChatUnsupportedApprovalsAndReservedOverlay() {
        tapRow("workspaceRow-\(FixtureID.workspace)")
        tapRow("spaceRow-\(FixtureID.space)")
        tapRow("sessionRow-\(FixtureID.session)")

        XCTAssertTrue(element("chatTranscript").waitForExistence(timeout: 10))
        XCTAssertTrue(element("messageRow-\(FixtureID.firstMessage)").waitForExistence(timeout: 5))

        let approvals = element("approvalsUnsupportedHeadline")
        XCTAssertTrue(approvals.exists)
        XCTAssertTrue(approvals.label.contains("unavailable"))
        XCTAssertFalse(app.buttons["Approve"].exists)
        XCTAssertFalse(app.buttons["Reject"].exists)

        XCTAssertTrue(element("overlaySlotReserved").exists)

        let composer = element("composerField")
        XCTAssertTrue(composer.exists)
        XCTAssertFalse(composer.isEnabled)
        XCTAssertTrue(element("composerDisabledNote").exists)
        XCTAssertTrue(element("composerSendButton").exists)
        XCTAssertFalse(element("composerSendButton").isEnabled)

        let agentChip = element("agentChip-\(FixtureID.protocolRepairAgent)")
        XCTAssertTrue(agentChip.exists)
        agentChip.tap()
        XCTAssertTrue(app.navigationBars["Protocol repair"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Structured approvals"].exists)
        XCTAssertTrue(app.staticTexts["Unavailable"].firstMatch.exists)
    }

    func testTrustedDevicesShellAndSimulatorCameraUnavailable() {
        let devicesButton = element("trustedDevicesButton")
        XCTAssertTrue(devicesButton.waitForExistence(timeout: 30))
        devicesButton.tap()

        XCTAssertTrue(element("pairingNotImplementedNote").waitForExistence(timeout: 10))
        XCTAssertTrue(element("trustedDeviceRow-\(FixtureID.studioMacDevice)").exists)

        let revoke = element("revokeButton-\(FixtureID.studioMacDevice)")
        XCTAssertTrue(revoke.exists)
        revoke.tap()
        XCTAssertTrue(revoke.waitForNonExistence(timeout: 5))

        let pairRow = element("pairDeviceRow")
        scrollUntilHittable(pairRow)
        pairRow.tap()

        XCTAssertTrue(element("pairingCameraUnavailable").waitForExistence(timeout: 10))
        let simulatorNote = app.staticTexts
            .matching(NSPredicate(format: "label CONTAINS %@", "Simulator"))
            .firstMatch
        XCTAssertTrue(simulatorNote.waitForExistence(timeout: 5))
        XCTAssertTrue(element("pairingSecurityNote").exists)
    }

    // MARK: - Helpers

    private func element(_ identifier: String) -> XCUIElement {
        app.descendants(matching: .any)
            .matching(identifier: identifier)
            .firstMatch
    }

    private func tapRow(_ identifier: String, file: StaticString = #filePath, line: UInt = #line) {
        let row = element(identifier)
        XCTAssertTrue(row.waitForExistence(timeout: 20), "missing row \(identifier)", file: file, line: line)
        scrollUntilHittable(row)
        row.tap()
    }

    private func scrollUntilHittable(_ element: XCUIElement, attempts: Int = 4) {
        var remaining = attempts
        while !element.isHittable && remaining > 0 {
            app.swipeUp()
            remaining -= 1
        }
    }
}
