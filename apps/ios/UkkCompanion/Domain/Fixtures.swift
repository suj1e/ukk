import Foundation

/// Deterministic, clearly labelled sample content. Fixture-first: this build
/// never contacts a Host, so every value here is local and reproducible.
struct FixtureCatalog: Equatable, Sendable {
    static let bannerText = "FIXTURE DATA — no Host connection"
    static let badgeLabel = "FIXTURE"
    static let disclaimer = "Deterministic sample content rendered locally. Nothing is sent to a Host and no Agent process is running."

    let workspaces: [Workspace]
    let devices: [TrustedDevice]

    static let fixture = FixtureCatalog(
        workspaces: [.fixture],
        devices: TrustedDevice.fixtureDevices
    )

    func workspace(_ id: WorkspaceID) -> Workspace? {
        workspaces.first { $0.id == id }
    }

    func space(_ id: SpaceID) -> Space? {
        for workspace in workspaces {
            if let space = workspace.space(id) { return space }
        }
        return nil
    }

    func session(_ id: SessionID) -> Session? {
        for workspace in workspaces {
            for space in workspace.spaces {
                if let session = space.session(id) { return session }
            }
        }
        return nil
    }

    func space(containing sessionID: SessionID) -> Space? {
        for workspace in workspaces {
            for space in workspace.spaces where space.session(sessionID) != nil {
                return space
            }
        }
        return nil
    }

    func agentRun(_ id: AgentRunID) -> AgentRun? {
        for workspace in workspaces {
            for space in workspace.spaces {
                for session in space.sessions {
                    if let agent = session.agentRun(id) { return agent }
                }
            }
        }
        return nil
    }

    var spaceCount: Int {
        workspaces.reduce(0) { $0 + $1.spaces.count }
    }

    var sessionCount: Int {
        workspaces.reduce(0) { partial, workspace in
            partial + workspace.spaces.reduce(0) { $0 + $1.sessions.count }
        }
    }
}

// MARK: - Fixed clock

enum FixtureTime {
    /// 2026-11-07T00:00:00Z. Fixed so transcripts render identically every launch.
    static let base = Date(timeIntervalSince1970: 1_794_048_000)

    static func minutes(_ value: Int) -> Date {
        base.addingTimeInterval(TimeInterval(value) * 60)
    }
}

// MARK: - Workspace / Spaces / Sessions

extension Workspace {
    static let fixture = Workspace(
        id: .fixture(1),
        name: "Fixture Workspace",
        hostLabel: "Studio Mac (fixture host, not connected)",
        spaces: [.ukk, .relaySpike],
        provenance: .fixture
    )
}

extension Space {
    static let ukk = Space(
        id: .fixture(101),
        name: "ukk",
        projectRoot: "/Users/fixture/dev/ukk",
        sessions: [.mockProtocol, .tabTransfer],
        provenance: .fixture
    )

    static let relaySpike = Space(
        id: .fixture(102),
        name: "relay-spike",
        projectRoot: "/Users/fixture/dev/alma-relay",
        sessions: [.relaySpike],
        provenance: .fixture
    )
}

extension Session {
    static let mockProtocol = Session(
        id: .fixture(201),
        spaceID: .fixture(101),
        title: "Fix flaky mock protocol",
        lifecycle: .active,
        agentRuns: [.protocolRepair, .independentReview],
        messages: [.instruction, .protocolRepairUpdate, .fixtureNotice, .reviewNote],
        capabilities: .fixture,
        provenance: .fixture
    )

    static let tabTransfer = Session(
        id: .fixture(202),
        spaceID: .fixture(101),
        title: "Pane tab transfer review",
        lifecycle: .paused,
        agentRuns: [.layoutReviewer],
        messages: [.tabInstruction, .tabReviewUpdate],
        capabilities: .fixture,
        provenance: .fixture
    )

    static let relaySpike = Session(
        id: .fixture(203),
        spaceID: .fixture(102),
        title: "Relay compatibility spike",
        lifecycle: .active,
        agentRuns: [.relaySpikeAgent],
        messages: [.relayNotice, .relayInstruction, .relayObservation],
        capabilities: .fixture,
        provenance: .fixture
    )
}

// MARK: - Agent runs

extension AgentRun {
    static let protocolRepair = AgentRun(
        id: .fixture(301),
        sessionID: .fixture(201),
        name: "Protocol repair",
        adapterKind: "codex",
        status: .running,
        statusSource: "structured adapter event (fixture)",
        terminalRef: TerminalRef(machine: "studio-mac", terminal: "term-7f2a"),
        provenance: .fixture
    )

    static let independentReview = AgentRun(
        id: .fixture(302),
        sessionID: .fixture(201),
        name: "Independent review",
        adapterKind: "claude-code",
        status: .blocked,
        statusSource: "adapter reports waiting (fixture)",
        terminalRef: TerminalRef(machine: "studio-mac", terminal: "term-9c10"),
        provenance: .fixture
    )

    static let layoutReviewer = AgentRun(
        id: .fixture(303),
        sessionID: .fixture(202),
        name: "Layout reviewer",
        adapterKind: "codex",
        status: .finished,
        statusSource: "reported completion (fixture)",
        terminalRef: TerminalRef(machine: "studio-mac", terminal: "term-2a11"),
        provenance: .fixture
    )

    static let relaySpikeAgent = AgentRun(
        id: .fixture(304),
        sessionID: .fixture(203),
        name: "Relay spike",
        adapterKind: "custom",
        status: .unknown,
        statusSource: "no fresh observation (fixture) — not evidence the process stopped",
        terminalRef: TerminalRef(machine: "headless-linux", terminal: "term-55aa"),
        provenance: .fixture
    )
}

// MARK: - Fixture transcript

extension ChatMessage {
    static let instruction = ChatMessage(
        id: .fixture(401),
        sessionID: .fixture(201),
        agentRunID: nil,
        role: .user,
        text: "The Ubuntu mock EOF test still fails. Reproduce it and explain the actual error before you change any assertion.",
        occurredAt: FixtureTime.minutes(0),
        provenance: .fixture
    )

    static let protocolRepairUpdate = ChatMessage(
        id: .fixture(402),
        sessionID: .fixture(201),
        agentRunID: .fixture(301),
        role: .agent,
        text: "Reproducing with the pinned mock server. The client surfaces `HcError::Protocol` only after the mock closes stdout; the test expects it earlier, at the delayed EOF.",
        occurredAt: FixtureTime.minutes(2),
        provenance: .fixture
    )

    static let fixtureNotice = ChatMessage(
        id: .fixture(403),
        sessionID: .fixture(201),
        agentRunID: nil,
        role: .system,
        text: "Fixture transcript. Rendered locally from deterministic sample data; no Host session was contacted and no command can be sent.",
        occurredAt: FixtureTime.minutes(3),
        provenance: .fixture
    )

    static let reviewNote = ChatMessage(
        id: .fixture(404),
        sessionID: .fixture(201),
        agentRunID: .fixture(302),
        role: .agent,
        text: "Review note (fixture): keep the assertion typed, and report the stream termination error instead of asserting on a timeout.",
        occurredAt: FixtureTime.minutes(5),
        provenance: .fixture
    )

    static let tabInstruction = ChatMessage(
        id: .fixture(405),
        sessionID: .fixture(202),
        agentRunID: nil,
        role: .user,
        text: "Move the tab, keep the same Session, and confirm the CAS revision is checked exactly once.",
        occurredAt: FixtureTime.minutes(20),
        provenance: .fixture
    )

    static let tabReviewUpdate = ChatMessage(
        id: .fixture(406),
        sessionID: .fixture(202),
        agentRunID: .fixture(303),
        role: .agent,
        text: "PaneNode.move_tab returns a new tree; the local capability-protected workspace.tab.move commits a single CAS revision. Fixture text — not a live result.",
        occurredAt: FixtureTime.minutes(21),
        provenance: .fixture
    )

    static let relayNotice = ChatMessage(
        id: .fixture(407),
        sessionID: .fixture(203),
        agentRunID: nil,
        role: .system,
        text: "This fixture agent has no fresh observation. Unknown is an observation state, not evidence that its process stopped.",
        occurredAt: FixtureTime.minutes(40),
        provenance: .fixture
    )

    static let relayInstruction = ChatMessage(
        id: .fixture(408),
        sessionID: .fixture(203),
        agentRunID: nil,
        role: .user,
        text: "Check relay compatibility against the pinned relay version and list what the handshake actually covers.",
        occurredAt: FixtureTime.minutes(41),
        provenance: .fixture
    )

    static let relayObservation = ChatMessage(
        id: .fixture(409),
        sessionID: .fixture(203),
        agentRunID: .fixture(304),
        role: .agent,
        text: "Last fixture observation: the handshake covered HTTP forwarding, not the pairing flow. No current Agent status is available.",
        occurredAt: FixtureTime.minutes(42),
        provenance: .fixture
    )
}
