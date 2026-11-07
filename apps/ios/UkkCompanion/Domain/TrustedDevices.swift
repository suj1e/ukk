import Foundation

/// Capability scopes are separate grants; pairing one device does not imply any
/// of them. Values match the documented Host protocol vocabulary.
enum DeviceScope: String, CaseIterable, Equatable, Sendable {
    case observe
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

struct TrustedDevice: Identifiable, Equatable, Sendable {
    let id: DeviceID
    let label: String
    let platform: String
    let role: DeviceRole
    let scopes: Set<DeviceScope>
    let presence: DevicePresence
    let lastSeenAt: Date?
    let isCurrentDevice: Bool
    let provenance: DataProvenance

    var scopesSummary: String {
        let ordered = DeviceScope.allCases.filter { scopes.contains($0) }
        guard !ordered.isEmpty else { return "No grants" }
        return ordered.map(\.label).joined(separator: " · ")
    }
}

enum DeviceRevocationOutcome: Equatable, Sendable {
    case revoked
    case alreadyRevoked
    case refusedCurrentDevice
    case notFound

    var message: String {
        switch self {
        case .revoked:
            return "Revoked locally in the fixture list. No Host request was sent."
        case .alreadyRevoked:
            return "This device is already revoked."
        case .refusedCurrentDevice:
            return "The current device cannot revoke itself. Use the Host from another trusted device."
        case .notFound:
            return "The device is no longer in the trusted list."
        }
    }
}

/// Local management shell. Mutation is limited to this in-memory fixture list;
/// real revocation needs the Host protocol (S1/S2) and is not implemented.
struct TrustedDeviceStore: Equatable, Sendable {
    private(set) var devices: [TrustedDevice]

    init(devices: [TrustedDevice]) {
        self.devices = devices
    }

    static let fixture = TrustedDeviceStore(devices: TrustedDevice.fixtureDevices)

    var currentDevice: TrustedDevice? {
        devices.first { $0.isCurrentDevice }
    }

    var sortedForDisplay: [TrustedDevice] {
        devices.sorted { lhs, rhs in
            if lhs.isCurrentDevice != rhs.isCurrentDevice {
                return lhs.isCurrentDevice
            }
            let lhsRevoked = lhs.presence == .revoked
            let rhsRevoked = rhs.presence == .revoked
            if lhsRevoked != rhsRevoked {
                return !lhsRevoked
            }
            return lhs.label < rhs.label
        }
    }

    mutating func revoke(_ id: DeviceID) -> DeviceRevocationOutcome {
        guard let index = devices.firstIndex(where: { $0.id == id }) else {
            return .notFound
        }
        let device = devices[index]
        if device.isCurrentDevice {
            return .refusedCurrentDevice
        }
        if device.presence == .revoked {
            return .alreadyRevoked
        }
        devices[index] = TrustedDevice(
            id: device.id,
            label: device.label,
            platform: device.platform,
            role: device.role,
            scopes: [],
            presence: .revoked,
            lastSeenAt: device.lastSeenAt,
            isCurrentDevice: device.isCurrentDevice,
            provenance: device.provenance
        )
        return .revoked
    }
}

extension TrustedDevice {
    static let fixtureDevices: [TrustedDevice] = [
        TrustedDevice(
            id: .fixture(501),
            label: "This iPhone (fixture)",
            platform: "iOS 27 · Simulator",
            role: .observer,
            scopes: [.observe],
            presence: .offline,
            lastSeenAt: nil,
            isCurrentDevice: true,
            provenance: .fixture
        ),
        TrustedDevice(
            id: .fixture(502),
            label: "Studio Mac",
            platform: "macOS",
            role: .operator,
            scopes: [.observe, .agentSend],
            presence: .online,
            lastSeenAt: FixtureTime.minutes(45),
            isCurrentDevice: false,
            provenance: .fixture
        ),
        TrustedDevice(
            id: .fixture(503),
            label: "Old iPad",
            platform: "iPadOS",
            role: .observer,
            scopes: [],
            presence: .revoked,
            lastSeenAt: FixtureTime.minutes(-9_000),
            isCurrentDevice: false,
            provenance: .fixture
        )
    ]
}
