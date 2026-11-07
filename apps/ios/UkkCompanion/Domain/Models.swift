import Foundation

/// Where a value came from. This build only ships fixture data; reading a real
/// Host is owned by the S1/S2 protocol lanes and is not implemented here.
enum DataProvenance: String, Equatable, Sendable {
    case fixture

    var label: String { "fixture" }
}

enum StatusTone: Equatable, Sendable {
    case neutral
    case positive
    case warning
    case negative
}

enum SessionLifecycle: String, CaseIterable, Equatable, Sendable {
    case active
    case paused
    case archived

    var label: String {
        switch self {
        case .active: return "Active"
        case .paused: return "Paused"
        case .archived: return "Archived"
        }
    }

    var tone: StatusTone {
        switch self {
        case .active: return .positive
        case .paused: return .warning
        case .archived: return .neutral
        }
    }
}

/// Observation states from the domain model. `unknown` is an observation, never
/// proof that the underlying process stopped.
enum AgentRunStatus: String, CaseIterable, Equatable, Sendable {
    case created
    case starting
    case running
    case blocked
    case finished
    case failed
    case cancelled
    case unknown

    var label: String {
        switch self {
        case .created: return "Created"
        case .starting: return "Starting"
        case .running: return "Running"
        case .blocked: return "Blocked"
        case .finished: return "Finished"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        case .unknown: return "Unknown"
        }
    }

    var tone: StatusTone {
        switch self {
        case .running, .finished: return .positive
        case .starting, .blocked, .created: return .warning
        case .failed, .cancelled: return .negative
        case .unknown: return .neutral
        }
    }
}

/// Binding to the herdr-core runtime. The value is copied, never inferred.
struct TerminalRef: Equatable, Sendable {
    let machine: String
    let terminal: String

    var display: String { "\(machine) / \(terminal)" }
}

struct AgentRun: Identifiable, Equatable, Sendable {
    let id: AgentRunID
    let sessionID: SessionID
    let name: String
    let adapterKind: String
    let status: AgentRunStatus
    /// Provenance of the status itself, e.g. "structured adapter event (fixture)".
    let statusSource: String
    let terminalRef: TerminalRef
    let provenance: DataProvenance
}

enum ChatRole: String, Equatable, Sendable {
    case user
    case agent
    case system
}

struct ChatMessage: Identifiable, Equatable, Sendable {
    let id: ChatMessageID
    let sessionID: SessionID
    let agentRunID: AgentRunID?
    let role: ChatRole
    let text: String
    let occurredAt: Date
    let provenance: DataProvenance
}

struct Session: Identifiable, Equatable, Sendable {
    let id: SessionID
    let spaceID: SpaceID
    let title: String
    let lifecycle: SessionLifecycle
    let agentRuns: [AgentRun]
    let messages: [ChatMessage]
    let capabilities: SessionCapabilities
    let provenance: DataProvenance

    func agentRun(_ id: AgentRunID) -> AgentRun? {
        agentRuns.first { $0.id == id }
    }

    func messages(for agentRunID: AgentRunID) -> [ChatMessage] {
        messages.filter { $0.agentRunID == agentRunID }
    }
}

struct Space: Identifiable, Equatable, Sendable {
    let id: SpaceID
    let name: String
    let projectRoot: String
    let sessions: [Session]
    let provenance: DataProvenance

    func session(_ id: SessionID) -> Session? {
        sessions.first { $0.id == id }
    }
}

struct Workspace: Identifiable, Equatable, Sendable {
    let id: WorkspaceID
    let name: String
    let hostLabel: String
    let spaces: [Space]
    let provenance: DataProvenance

    func space(_ id: SpaceID) -> Space? {
        spaces.first { $0.id == id }
    }
}
