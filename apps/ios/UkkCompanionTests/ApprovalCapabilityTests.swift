import XCTest
@testable import UkkCompanion

final class ApprovalCapabilityTests: XCTestCase {
    func testFixtureApprovalsAreExplicitlyUnsupported() {
        let panel = SessionCapabilities.fixture.approvalPanel
        XCTAssertEqual(panel.capability, .unsupported(.noNegotiatedHostCapability))
        XCTAssertFalse(panel.allowsDecision)
        XCTAssertTrue(panel.actions.isEmpty)
        XCTAssertEqual(panel.headline, "Approvals unavailable")
        XCTAssertTrue(panel.detail.contains("Terminal text is never parsed"))
    }

    func testEvenAdvertisedApprovalsRemainUnsupportedWithoutVerifiedChannel() {
        let capabilities = SessionCapabilities(
            structuredApprovals: true,
            agentSend: true,
            terminalAttach: true,
            screenPreview: true,
            screenControl: true
        )
        let panel = capabilities.approvalPanel
        XCTAssertEqual(panel.capability, .unsupported(.noVerifiedAdapterChannel))
        XCTAssertFalse(panel.allowsDecision)
        XCTAssertTrue(panel.actions.isEmpty)
    }

    func testNoApprovalActionCanEverBeProduced() {
        for reason in [ApprovalUnsupportedReason.noNegotiatedHostCapability, .noVerifiedAdapterChannel] {
            let panel = ApprovalPanelState(capability: .unsupported(reason))
            XCTAssertTrue(panel.actions.isEmpty)
            XCTAssertFalse(panel.allowsDecision)
        }
    }

    func testComposerStaysReadOnlyInThisBuild() {
        let capabilities = SessionCapabilities(
            structuredApprovals: false,
            agentSend: true,
            terminalAttach: true,
            screenPreview: false,
            screenControl: false
        )
        let composer = capabilities.composerState
        XCTAssertFalse(composer.isEnabled)
        XCTAssertTrue(composer.note.contains("Sending is disabled"))
        XCTAssertTrue(composer.note.contains("agent.send"))
    }

    func testRemoteScreenSlotIsReservedAndNotInteractive() {
        let slot = SessionOverlayState.i1Reserved
        XCTAssertFalse(slot.isInteractive)
        XCTAssertEqual(slot.title, "Remote Screen")
        XCTAssertTrue(slot.detail.contains("M1"))
        XCTAssertTrue(slot.detail.contains("no capture"))
    }

    func testAllSessionFixturesShowTheSameUnsupportedApprovalState() {
        for workspace in FixtureCatalog.fixture.workspaces {
            for space in workspace.spaces {
                for session in space.sessions {
                    let panel = session.capabilities.approvalPanel
                    XCTAssertFalse(panel.allowsDecision)
                    XCTAssertTrue(panel.actions.isEmpty)
                }
            }
        }
    }
}
