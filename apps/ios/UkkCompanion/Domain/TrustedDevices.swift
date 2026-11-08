import Foundation

/// Capability scopes are separate grants; pairing one device does not imply any
/// of them. Raw values match the Host protocol vocabulary.
enum DeviceScope: String, CaseIterable, Equatable, Sendable {
    case observe = "session.observe"
    case agentSend = "agent.send"
    case agentApprove = "agent.approve"
    case workspaceWrite = "workspace.write"
    case screenView = "screen.view"
    case screenControl = "screen.control"
    case filesystemTransfer = "filesystem.transfer"

    var label: String {
        switch self {
        case .observe: return "Observe sessions"
        case .agentSend: return "Send to Agent"
        case .agentApprove: return "Approve actions"
        case .workspaceWrite: return "Edit workspace"
        case .screenView: return "View screen"
        case .screenControl: return "Control input"
        case .filesystemTransfer: return "Transfer files"
        }
    }
}

enum DeviceRole: String, Equatable, Sendable {
    case observer
    case `operator`

    var label: String {
        switch self {
        case .observer: return "Observer"
        case .operator: return "Operator"
        }
    }
}

enum DevicePresence: String, Equatable, Sendable {
    case online
    case offline
    case revoked

    var label: String {
        switch self {
        case .online: return "Online"
        case .offline: return "Offline"
        case .revoked: return "Revoked"
        }
    }

    var tone: StatusTone {
        switch self {
        case .online: return .positive
        case .offline: return .neutral
        case .revoked: return .negative
        }
    }
}

/// Display model projected from `remote.status`. This is the Host's device list,
/// not local fixture state.
struct TrustedDevice: Identifiable, Equatable, Sendable {
    let id: DeviceID
    /// The Host's raw device id, used for `remote.revoke`.
    let remoteID: String
    let label: String
    let platform: String
    let role: DeviceRole
    let scopes: Set<DeviceScope>
    let presence: DevicePresence
    let lastSeenAt: Date?
    let grantedUntil: Date?
    let isCurrentDevice: Bool
    let provenance: DataProvenance

    var scopesSummary: String {
        let ordered = DeviceScope.allCases.filter { scopes.contains($0) }
        guard !ordered.isEmpty else { return "No grants" }
        return ordered.map(\.label).joined(separator: " · ")
    }

    init(remote: RemoteDevice, currentDeviceID: String) {
        let granted = Set(remote.scopes.compactMap(DeviceScope.init(rawValue:)))
        self.remoteID = remote.id
        self.id = DeviceID.fromHost(remote.id)
        self.label = remote.label
        self.platform = "Paired device"
        self.role = granted.contains(.agentSend) || granted.contains(.screenControl) ? .operator : .observer
        self.scopes = granted
        self.presence = remote.revoked ? .revoked : .online
        self.lastSeenAt = nil
        self.grantedUntil = remote.expiresMS.map { Date(timeIntervalSince1970: Double($0) / 1000) }
        self.isCurrentDevice = remote.id == currentDeviceID
        self.provenance = .live
    }
}
