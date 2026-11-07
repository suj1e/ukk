import XCTest
@testable import UkkCompanion

final class NavigationStateTests: XCTestCase {
    func testStartsAtWorkspaceRoot() {
        let state = NavigationState()
        XCTAssertEqual(state.depth, 0)
        XCTAssertNil(state.selectedWorkspaceID)
        XCTAssertNil(state.selectedSpaceID)
        XCTAssertNil(state.selectedSessionID)
        XCTAssertNil(state.selectedAgentRunID)
    }

    func testFullWorkspaceHierarchyIsRecordedInOrder() {
        var state = NavigationState()
        let workspaceID = WorkspaceID.fixture(1)
        let spaceID = SpaceID.fixture(101)
        let sessionID = SessionID.fixture(201)
        let agentRunID = AgentRunID.fixture(301)

        state.openWorkspace(workspaceID)
        state.openSpace(spaceID)
        state.openSession(sessionID)
        state.openAgentRun(agentRunID)

        XCTAssertEqual(state.path, [
            .workspace(workspaceID),
            .space(spaceID),
            .session(sessionID),
            .agentRun(agentRunID)
        ])
        XCTAssertEqual(state.depth, 4)
        XCTAssertEqual(state.selectedWorkspaceID, workspaceID)
        XCTAssertEqual(state.selectedSpaceID, spaceID)
        XCTAssertEqual(state.selectedSessionID, sessionID)
        XCTAssertEqual(state.selectedAgentRunID, agentRunID)
    }

    func testRepeatedPushOfTopRouteIsIgnored() {
        var state = NavigationState()
        let sessionID = SessionID.fixture(201)
        state.openSession(sessionID)
        state.openSession(sessionID)
        XCTAssertEqual(state.path, [.session(sessionID)])
    }

    func testOpeningAnotherWorkspaceReplacesStack() {
        var state = NavigationState()
        state.openWorkspace(.fixture(1))
        state.openSpace(.fixture(101))
        state.openSession(.fixture(201))

        state.openWorkspace(.fixture(2))

        XCTAssertEqual(state.path, [.workspace(.fixture(2))])
        XCTAssertNil(state.selectedSpaceID)
    }

    func testPopToRootClearsEverything() {
        var state = NavigationState()
        state.openWorkspace(.fixture(1))
        state.openSpace(.fixture(101))
        state.openSession(.fixture(201))
        state.popToRoot()
        XCTAssertTrue(state.path.isEmpty)
        XCTAssertEqual(state.depth, 0)
    }

    func testWorkspaceSelectionIsDeepestMatch() {
        var state = NavigationState()
        state.openWorkspace(.fixture(1))
        state.openSpace(.fixture(101))
        state.openWorkspace(.fixture(1))
        state.openSpace(.fixture(102))
        XCTAssertEqual(state.selectedWorkspaceID, .fixture(1))
        XCTAssertEqual(state.selectedSpaceID, .fixture(102))
        XCTAssertNil(state.selectedSessionID)
    }
}
