import Foundation

/// Where a value came from. Fixture values are deterministic local samples; live
/// values were produced by an authenticated Host RPC and may be stale while the
/// connection is degraded or offline.
enum DataProvenance: String, Equatable, Sendable {
    case fixture
    case live

    var label: String {
        switch self {
        case .fixture: return "fixture"
        case .live: return "host"
        }
    }
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

    init(providerState: String) {
        switch providerState.lowercased() {
        case "created": self = .created
        case "starting": self = .starting
        case "running": self = .running
        case "blocked": self = .blocked
        case "finished", "completed": self = .finished
        case "failed", "error": self = .failed
        case "cancelled", "canceled": self = .cancelled
        default: self = .unknown
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
    /// Provenance of the status itself, e.g. "provider snapshot (codex)".
    let statusSource: String
    let terminalRef: TerminalRef
    let provenance: DataProvenance
}

enum ChatRole: String, Equatable, Sendable {
    case user
    case agent
    case system

    init(wireRole: String) {
        switch wireRole.lowercased() {
        case "user": self = .user
        case "assistant", "agent": self = .agent
        default: self = .system
        }
    }
}

struct ChatMessage: Identifiable, Equatable, Sendable {
    let id: ChatMessageID
    let sessionID: SessionID
    let agentRunID: AgentRunID?
    let role: ChatRole
    let text: String
    let isFinal: Bool
    let occurredAt: Date
    let provenance: DataProvenance

    init(
        id: ChatMessageID,
        sessionID: SessionID,
        agentRunID: AgentRunID?,
        role: ChatRole,
        text: String,
        isFinal: Bool = true,
        occurredAt: Date,
        provenance: DataProvenance
    ) {
        self.id = id
        self.sessionID = sessionID
        self.agentRunID = agentRunID
        self.role = role
        self.text = text
        self.isFinal = isFinal
        self.occurredAt = occurredAt
        self.provenance = provenance
    }
}

/// One provider-reported tool call. Tool calls are shown only when the provider
/// emitted them; they are never reconstructed from terminal text.
struct ToolCall: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let status: String
    let input: String?
    let output: String?
    let isError: Bool
}

/// One provider-emitted approval request.
struct ApprovalRequest: Identifiable, Equatable, Sendable {
    let id: String
    let toolName: String
    let input: String?
    let state: String
    let expiresAt: Date?

    var isPending: Bool { state.lowercased() == "pending" }
}

struct Session: Identifiable, Equatable, Sendable {
    let id: SessionID
    let spaceID: SpaceID
    let title: String
    let lifecycle: SessionLifecycle
    let agentRuns: [AgentRun]
    let messages: [ChatMessage]
    let tools: [ToolCall]
    let approvals: [ApprovalRequest]
    let capabilities: SessionCapabilities
    /// The Host's opaque Session id. Absent for fixture sessions.
    let rpcID: String?
    let provenance: DataProvenance

    init(
        id: SessionID,
        spaceID: SpaceID,
        title: String,
        lifecycle: SessionLifecycle,
        agentRuns: [AgentRun],
        messages: [ChatMessage],
        tools: [ToolCall] = [],
        approvals: [ApprovalRequest] = [],
        capabilities: SessionCapabilities,
        rpcID: String? = nil,
        provenance: DataProvenance
    ) {
        self.id = id
        self.spaceID = spaceID
        self.title = title
        self.lifecycle = lifecycle
        self.agentRuns = agentRuns
        self.messages = messages
        self.tools = tools
        self.approvals = approvals
        self.capabilities = capabilities
        self.rpcID = rpcID
        self.provenance = provenance
    }

    func agentRun(_ id: AgentRunID) -> AgentRun? {
        agentRuns.first { $0.id == id }
    }

    func messages(for agentRunID: AgentRunID) -> [ChatMessage] {
        messages.filter { $0.agentRunID == agentRunID }
    }

    var pendingApprovals: [ApprovalRequest] {
        approvals.filter(\.isPending)
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

// MARK: - Stable identifiers for Host strings

extension EntityID {
    /// Maps a Host id string to a stable typed identifier without losing UUID
    /// inputs. Non-UUID identifiers are hashed so navigation stays stable across
    /// refreshes.
    static func fromHost(_ value: String) -> EntityID<Tag> {
        if let uuid = UUID(uuidString: value) {
            return EntityID(rawValue: uuid)
        }
        let digest = HandshakeCrypto.sha256(Data(value.utf8))
        var bytes = Array(digest.prefix(16))
        bytes[6] = (bytes[6] & 0x0F) | 0x40
        bytes[8] = (bytes[8] & 0x3F) | 0x80
        let uuid = uuidFromBytes(bytes)
        return EntityID(rawValue: uuid)
    }
}

private func uuidFromBytes(_ bytes: [UInt8]) -> UUID {
    let tuple: uuid_t = (
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12], bytes[13], bytes[14], bytes[15]
    )
    return UUID(uuid: tuple)
}
