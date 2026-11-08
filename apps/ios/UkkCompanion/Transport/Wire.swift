import Foundation

// MARK: - JSON value

/// A lossless-enough JSON value used for request parameters and untyped
/// results. Unknown result fields remain available without a schema change.
enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "unsupported JSON value")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    static func object(_ fields: [(String, JSONValue?)]) -> JSONValue {
        var result: [String: JSONValue] = [:]
        for (key, value) in fields {
            if let value { result[key] = value }
        }
        return .object(result)
    }
}

// MARK: - Methods

enum WireMethod: String, CaseIterable, Sendable {
    case health
    case workspaceGet = "workspace.get"
    case agentStart = "agent.start"
    case agentSnapshot = "agent.snapshot"
    case agentList = "agent.list"
    case agentPrompt = "agent.prompt"
    case agentAbort = "agent.abort"
    case agentStop = "agent.stop"
    case agentApprove = "agent.approve"
    case repoStatus = "repo.status"
    case remoteStatus = "remote.status"
    case remoteRevoke = "remote.revoke"
    case screenStart = "screen.start"
    case screenAnswer = "screen.answer"
    case screenStop = "screen.stop"
    case screenInput = "screen.input"
}

// MARK: - Request / response

struct WireRequest: Encodable {
    let id: String
    let method: String
    let params: JSONValue?

    enum CodingKeys: String, CodingKey {
        case id, method, params
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(method, forKey: .method)
        if let params {
            try container.encode(params, forKey: .params)
        }
    }
}

struct WireError: Error, Codable, Equatable, Sendable {
    let code: String
    let message: String
}

struct WireResponse: Decodable {
    let id: String
    let result: JSONValue?
    let error: WireError?

    enum CodingKeys: String, CodingKey {
        case id, result, error
    }
}

struct WireEventEnvelope: Decodable {
    let type: String?
    let event: WireEvent
}

struct WireEvent: Decodable, Equatable, Sendable {
    let type: String?
    let sessionID: String?
    let agent: AgentSnapshot?
    let error: String?
    let atMS: Int64?

    enum CodingKeys: String, CodingKey {
        case type
        case sessionID = "session_id"
        case agent
        case error
        case atMS = "at_ms"
    }
}

// MARK: - Agent snapshot (apps/desktop/internal/bridge/wire.go)

struct AgentMessage: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let role: String
    let text: String
    let isFinal: Bool
    let createdMS: Int64

    enum CodingKeys: String, CodingKey {
        case id, role, text
        case isFinal = "final"
        case createdMS = "created_ms"
    }
}

struct AgentTool: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let turnID: String?
    let name: String
    let input: JSONValue?
    let output: JSONValue?
    let status: String
    let isError: Bool
    let startedMS: Int64?
    let endedMS: Int64?

    enum CodingKeys: String, CodingKey {
        case id, name, input, output, status
        case turnID = "turn_id"
        case isError = "is_error"
        case startedMS = "started_ms"
        case endedMS = "ended_ms"
    }
}

struct AgentApproval: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let turnID: String?
    let toolName: String
    let input: JSONValue?
    let state: String
    let expiresMS: Int64?

    enum CodingKeys: String, CodingKey {
        case id, input, state
        case turnID = "turn_id"
        case toolName = "tool_name"
        case expiresMS = "expires_ms"
    }
}

struct AgentTurn: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let state: String
    let startedMS: Int64?
    let endedMS: Int64?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case id, state, error
        case startedMS = "started_ms"
        case endedMS = "ended_ms"
    }
}

struct AgentUsage: Decodable, Equatable, Sendable {
    let promptTokens: Int64?
    let completionTokens: Int64?
    let totalTokens: Int64?

    enum CodingKeys: String, CodingKey {
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case totalTokens = "total_tokens"
    }
}

struct AgentSnapshot: Decodable, Equatable, Sendable, Identifiable {
    let provider: String
    let providerSessionID: String?
    let state: String
    let turns: [AgentTurn]
    let messages: [AgentMessage]
    let tools: [AgentTool]
    let pendingApprovals: [AgentApproval]
    let usage: AgentUsage?
    let error: String?
    let provenance: String?
    let updatedMS: Int64?
    /// Present on pushed events where the Host mirrors the Session id into the
    /// snapshot; absent from `agent.list` items in the current wire.
    let sessionID: String?

    var id: String { sessionID ?? providerSessionID ?? "agent" }

    enum CodingKeys: String, CodingKey {
        case provider, state, turns, messages, tools, usage, error, provenance
        case providerSessionID = "provider_session_id"
        case pendingApprovals = "pending_approvals"
        case updatedMS = "updated_ms"
        case sessionID = "session_id"
    }
}

// MARK: - Workspace snapshot (apps/desktop/internal/workspace/model.go)

struct WorkspaceSnapshot: Decodable, Equatable, Sendable {
    let revision: Int64
    let spaces: [SpaceRecord]
    let sessions: [SessionRecord]
    let agents: [AgentRecord]
}

struct SpaceRecord: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let title: String
    let projectRoot: String
    let createdMS: Int64?

    enum CodingKeys: String, CodingKey {
        case id, title
        case projectRoot = "project_root"
        case createdMS = "created_ms"
    }
}

struct SessionRecord: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let spaceID: String
    let title: String
    let lifecycle: String
    let createdMS: Int64?
    let updatedMS: Int64?

    enum CodingKeys: String, CodingKey {
        case id, title, lifecycle
        case spaceID = "space_id"
        case createdMS = "created_ms"
        case updatedMS = "updated_ms"
    }
}

struct AgentRecord: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let sessionID: String
    let parentID: String?
    let coreID: String
    let terminalAddress: String
    let adapter: String
    let status: String
    let createdMS: Int64?

    enum CodingKeys: String, CodingKey {
        case id, adapter, status
        case sessionID = "session_id"
        case parentID = "parent_id"
        case coreID = "core_id"
        case terminalAddress = "terminal_address"
        case createdMS = "created_ms"
    }
}

// MARK: - Remote status

struct RemoteStatus: Decodable, Equatable, Sendable {
    let hostID: String
    let relayConnected: Bool
    let relayURL: String?
    let devices: [RemoteDevice]
    let version: Int

    enum CodingKeys: String, CodingKey {
        case devices, version
        case hostID = "host_id"
        case relayConnected = "relay_connected"
        case relayURL = "relay_url"
    }
}

struct RemoteDevice: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let label: String
    let scopes: [String]
    let expiresMS: Int64?
    let revoked: Bool
    let createdMS: Int64

    enum CodingKeys: String, CodingKey {
        case id, label, scopes, revoked
        case expiresMS = "expires_ms"
        case createdMS = "created_ms"
    }
}

// MARK: - Screen

struct IceServerRecord: Decodable, Equatable, Sendable {
    let urls: [String]
    let username: String?
    let credential: String?

    enum CodingKeys: String, CodingKey {
        case urls, username, credential
    }
}

struct ScreenSessionRecord: Decodable, Equatable, Sendable {
    let sessionID: String
    let sdp: String
    let iceServers: [IceServerRecord]
    let view: Bool
    let control: Bool

    enum CodingKeys: String, CodingKey {
        case sdp, view, control
        case sessionID = "session_id"
        case iceServers = "ice_servers"
    }
}

struct ScreenInputEvent: Encodable, Equatable, Sendable {
    let kind: String
    let x: Double?
    let y: Double?
    let button: Int?
    let deltaX: Double?
    let deltaY: Double?
    let code: String?
    let down: Bool?

    enum CodingKeys: String, CodingKey {
        case kind, x, y, button, code, down
        case deltaX = "delta_x"
        case deltaY = "delta_y"
    }

    static func pointer(x: Double, y: Double, button: Int, down: Bool) -> ScreenInputEvent {
        ScreenInputEvent(kind: "pointer", x: x, y: y, button: button, deltaX: nil, deltaY: nil, code: nil, down: down)
    }

    static func wheel(deltaX: Double, deltaY: Double) -> ScreenInputEvent {
        ScreenInputEvent(kind: "wheel", x: nil, y: nil, button: nil, deltaX: deltaX, deltaY: deltaY, code: nil, down: nil)
    }

    static func key(code: String, down: Bool) -> ScreenInputEvent {
        ScreenInputEvent(kind: "key", x: nil, y: nil, button: nil, deltaX: nil, deltaY: nil, code: code, down: down)
    }
}

// MARK: - Parameter helpers

enum WireParams {
    static func session(_ sessionID: String) -> JSONValue {
        .object(["session_id": .string(sessionID)])
    }

    static func prompt(sessionID: String, text: String) -> JSONValue {
        .object(["session_id": .string(sessionID), "text": .string(text)])
    }

    static func approval(sessionID: String, approvalID: String, approve: Bool, note: String?) -> JSONValue {
        var object: [String: JSONValue] = [
            "session_id": .string(sessionID),
            "approval_id": .string(approvalID),
            "decision": .string(approve ? "approve" : "deny")
        ]
        if let note { object["note"] = .string(note) }
        return .object(object)
    }

    static func revoke(deviceID: String) -> JSONValue {
        .object(["device_id": .string(deviceID)])
    }

    static func screenStart(sessionID: String, view: Bool, control: Bool) -> JSONValue {
        .object([
            "session_id": .string(sessionID),
            "view": .bool(view),
            "control": .bool(control)
        ])
    }

    static func screenAnswer(sessionID: String, sdp: String) -> JSONValue {
        .object(["session_id": .string(sessionID), "sdp": .string(sdp)])
    }

    static func screenInput(sessionID: String, events: [ScreenInputEvent]) -> JSONValue {
        let encoded = (try? JSONEncoder().encode(events)) ?? Data()
        let value = (try? JSONDecoder().decode(JSONValue.self, from: encoded)) ?? .array([])
        return .object(["session_id": .string(sessionID), "events": value])
    }
}
