import XCTest
@testable import UkkCompanion

final class FixtureIntegrityTests: XCTestCase {
    private let catalog = FixtureCatalog.fixture

    func testCatalogueIsDeterministic() {
        XCTAssertEqual(FixtureCatalog.fixture, catalog)
        XCTAssertEqual(SessionID.fixture(201).description, "0A11CE00-0000-4000-8000-000000000201")
        XCTAssertEqual(AgentRunID.fixture(301).description, "0A11CE00-0000-4000-8000-000000000301")
    }

    func testFixtureLabellingIsUnmistakable() {
        XCTAssertTrue(FixtureCatalog.bannerText.contains("FIXTURE"))
        XCTAssertEqual(FixtureCatalog.badgeLabel, "FIXTURE")
        XCTAssertTrue(FixtureCatalog.disclaimer.contains("Nothing is sent to a Host"))
    }

    func testEveryEntityIsMarkedAsFixture() {
        for workspace in catalog.workspaces {
            XCTAssertEqual(workspace.provenance, .fixture)
            for space in workspace.spaces {
                XCTAssertEqual(space.provenance, .fixture)
                for session in space.sessions {
                    XCTAssertEqual(session.provenance, .fixture)
                    for agent in session.agentRuns {
                        XCTAssertEqual(agent.provenance, .fixture)
                        XCTAssertFalse(agent.statusSource.isEmpty)
                        XCTAssertFalse(agent.terminalRef.machine.isEmpty)
                        XCTAssertFalse(agent.terminalRef.terminal.isEmpty)
                    }
                    for message in session.messages {
                        XCTAssertEqual(message.provenance, .fixture)
                    }
                }
            }
        }
    }

    func testRelationshipsResolve() {
        XCTAssertEqual(catalog.spaceCount, 2)
        XCTAssertEqual(catalog.sessionCount, 3)

        for workspace in catalog.workspaces {
            for space in workspace.spaces {
                XCTAssertEqual(catalog.workspace(workspace.id), workspace)
                XCTAssertEqual(catalog.space(space.id), space)
                for session in space.sessions {
                    XCTAssertEqual(session.spaceID, space.id)
                    XCTAssertEqual(catalog.session(session.id), session)
                    XCTAssertEqual(catalog.space(containing: session.id), space)
                    for agent in session.agentRuns {
                        XCTAssertEqual(agent.sessionID, session.id)
                        XCTAssertEqual(catalog.agentRun(agent.id), agent)
                        XCTAssertNotNil(session.agentRun(agent.id))
                    }
                    for message in session.messages {
                        XCTAssertEqual(message.sessionID, session.id)
                        if let agentRunID = message.agentRunID {
                            XCTAssertNotNil(session.agentRun(agentRunID))
                        }
                    }
                }
            }
        }
    }

    func testMessagesAreOrderedPerSession() {
        for workspace in catalog.workspaces {
            for space in workspace.spaces {
                for session in space.sessions {
                    let timestamps = session.messages.map(\.occurredAt)
                    XCTAssertEqual(timestamps, timestamps.sorted())
                }
            }
        }
    }

    func testFixtureSystemNoticeStatesThatNoHostWasContacted() throws {
        let session = try XCTUnwrap(catalog.session(.fixture(201)))
        let systemMessages = session.messages.filter { $0.role == .system }
        XCTAssertFalse(systemMessages.isEmpty)
        XCTAssertTrue(systemMessages[0].text.contains("Fixture transcript"))
        XCTAssertTrue(systemMessages[0].text.contains("no Host session was contacted"))
        XCTAssertEqual(systemMessages[0].provenance, .fixture)
    }

    func testNoFixtureRowCanClaimLiveProvenance() {
        for workspace in catalog.workspaces {
            for space in workspace.spaces {
                for session in space.sessions {
                    XCTAssertEqual(session.provenance, .fixture)
                    XCTAssertNil(session.rpcID)
                    XCTAssertFalse(session.messages.contains { $0.provenance == .live })
                    XCTAssertFalse(session.agentRuns.contains { $0.provenance == .live })
                }
            }
        }
    }

    func testFixtureCapabilitiesAreAllUnnegotiated() throws {
        let session = try XCTUnwrap(catalog.session(.fixture(201)))
        XCTAssertEqual(session.capabilities, .unnegotiated)
        XCTAssertFalse(session.capabilities.agentSend)
        XCTAssertFalse(session.capabilities.structuredApprovals)
        XCTAssertFalse(session.capabilities.screenPreview)
        XCTAssertFalse(session.capabilities.screenControl)
        XCTAssertFalse(session.capabilities.terminalAttach)
    }

    func testUnknownStatusIsKeptAsObservation() throws {
        let agent = try XCTUnwrap(catalog.agentRun(.fixture(304)))
        XCTAssertEqual(agent.status, .unknown)
        XCTAssertTrue(agent.statusSource.contains("not evidence the process stopped"))
    }
}
