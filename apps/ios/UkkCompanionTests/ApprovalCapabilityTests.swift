import XCTest
@testable import UkkCompanion

final class ApprovalCapabilityTests: XCTestCase {
    func testFixtureApprovalsAreExplicitlyUnsupported() {
        let panel = SessionCapabilities.fixture.approvalPanel(pendingCount: 0)
        XCTAssertEqual(panel.capability, .unsupported(.noNegotiatedHostCapability))
        XCTAssertFalse(panel.allowsDecision)
        XCTAssertTrue(panel.actions.isEmpty)
        XCTAssertEqual(panel.headline, "Approvals unavailable")
        XCTAssertTrue(panel.detail.contains("Terminal text is never parsed"))
    }

    func testGrantedApprovalsWithoutProviderRequestOfferNoAction() {
        let capabilities = SessionCapabilities(
            structuredApprovals: true,
            agentSend: true,
            terminalAttach: false,
            screenPreview: false,
            screenControl: false
        )
        let panel = capabilities.approvalPanel(pendingCount: 0)
        XCTAssertEqual(panel.capability, .unsupported(.noPendingProviderApproval))
        XCTAssertFalse(panel.allowsDecision)
        XCTAssertTrue(panel.actions.isEmpty)
    }

    func testGrantedApprovalsWithProviderRequestCanBeDecided() {
        let capabilities = SessionCapabilities(
            structuredApprovals: true,
            agentSend: true,
            terminalAttach: false,
            screenPreview: false,
            screenControl: false
        )
        let panel = capabilities.approvalPanel(pendingCount: 2)
        XCTAssertEqual(panel.capability, .available)
        XCTAssertTrue(panel.allowsDecision)
        XCTAssertEqual(panel.actions, [.approve, .reject])
        XCTAssertEqual(panel.headline, "2 approvals waiting")
    }

    func testPendingApprovalWithoutGrantStaysUnsupported() {
        let capabilities = SessionCapabilities(
            structuredApprovals: false,
            agentSend: true,
            terminalAttach: false,
            screenPreview: false,
            screenControl: false
        )
        let panel = capabilities.approvalPanel(pendingCount: 3)
        XCTAssertEqual(panel.capability, .unsupported(.noNegotiatedHostCapability))
        XCTAssertTrue(panel.actions.isEmpty)
    }

    func testComposerRequiresLiveConnectionAndAgentSendGrant() {
        let noGrant = SessionCapabilities(
            structuredApprovals: false,
            agentSend: false,
            terminalAttach: false,
            screenPreview: false,
            screenControl: false
        )
        XCTAssertFalse(noGrant.composerState(isLive: true).isEnabled)
        XCTAssertTrue(noGrant.composerState(isLive: true).note.contains("agent.send"))

        let granted = SessionCapabilities(
            structuredApprovals: false,
            agentSend: true,
            terminalAttach: false,
            screenPreview: false,
            screenControl: false
        )
        XCTAssertFalse(granted.composerState(isLive: false).isEnabled)
        XCTAssertTrue(granted.composerState(isLive: false).note.contains("no authenticated Host connection"))
        XCTAssertTrue(granted.composerState(isLive: true).isEnabled)
    }

    func testCapabilitiesComeFromScopes() {
        let capabilities = SessionCapabilities(scopes: [.observe, .agentSend, .screenView])
        XCTAssertTrue(capabilities.agentSend)
        XCTAssertTrue(capabilities.screenPreview)
        XCTAssertFalse(capabilities.structuredApprovals)
        XCTAssertFalse(capabilities.screenControl)
        XCTAssertFalse(capabilities.terminalAttach)
    }

    func testRemoteScreenSlotIsReservedOnlyWithoutHost() {
        let slot = SessionOverlayState.i1Reserved
        XCTAssertFalse(slot.isInteractive)
        XCTAssertEqual(slot.title, "Remote Screen")
        XCTAssertTrue(slot.detail.contains("No Host"))
    }

    func testAllSessionFixturesStayUnnegotiated() {
        for workspace in FixtureCatalog.fixture.workspaces {
            for space in workspace.spaces {
                for session in space.sessions {
                    let panel = session.capabilities.approvalPanel(pendingCount: session.pendingApprovals.count)
                    XCTAssertFalse(panel.allowsDecision)
                    XCTAssertTrue(panel.actions.isEmpty)
                }
            }
        }
    }
}
