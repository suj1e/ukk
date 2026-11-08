import Foundation

/// Capabilities negotiated with a Host. These are derived from the device's
/// live grants reported by `remote.status`; pairing alone grants nothing.
struct SessionCapabilities: Equatable, Sendable {
    let structuredApprovals: Bool
    let agentSend: Bool
    let terminalAttach: Bool
    let screenPreview: Bool
    let screenControl: Bool

    /// The only value a no-Host build can truthfully produce.
    static let unnegotiated = SessionCapabilities(
        structuredApprovals: false,
        agentSend: false,
        terminalAttach: false,
        screenPreview: false,
        screenControl: false
    )

    static let fixture = SessionCapabilities.unnegotiated

    init(
        structuredApprovals: Bool,
        agentSend: Bool,
        terminalAttach: Bool,
        screenPreview: Bool,
        screenControl: Bool
    ) {
        self.structuredApprovals = structuredApprovals
        self.agentSend = agentSend
        self.terminalAttach = terminalAttach
        self.screenPreview = screenPreview
        self.screenControl = screenControl
    }

    init(scopes: Set<DeviceScope>) {
        self.structuredApprovals = scopes.contains(.agentApprove)
        self.agentSend = scopes.contains(.agentSend)
        self.terminalAttach = false
        self.screenPreview = scopes.contains(.screenView)
        self.screenControl = scopes.contains(.screenControl)
    }
}

enum ApprovalUnsupportedReason: Equatable, Sendable {
    /// The Host has not granted `agent.approve` to this device.
    case noNegotiatedHostCapability
    /// A grant exists but the Agent provider has emitted no approval request.
    case noPendingProviderApproval

    var headline: String { "Approvals unavailable" }

    var detail: String {
        switch self {
        case .noNegotiatedHostCapability:
            return "This device has no agent.approve grant. Terminal text is never parsed into approvals."
        case .noPendingProviderApproval:
            return "No provider-emitted approval is pending for this Session. Approvals are never inferred from terminal text."
        }
    }
}

/// Approvals are available only when the Host granted `agent.approve` and the
/// provider emitted a pending request.
enum ApprovalCapability: Equatable, Sendable {
    case available
    case unsupported(ApprovalUnsupportedReason)
}

enum ApprovalAction: String, Equatable, Sendable, CaseIterable {
    case approve
    case reject

    var label: String {
        switch self {
        case .approve: return "Approve"
        case .reject: return "Deny"
        }
    }
}

struct ApprovalPanelState: Equatable, Sendable {
    let capability: ApprovalCapability
    let pendingCount: Int

    init(capability: ApprovalCapability, pendingCount: Int = 0) {
        self.capability = capability
        self.pendingCount = pendingCount
    }

    var actions: [ApprovalAction] {
        switch capability {
        case .available:
            return pendingCount > 0 ? [.approve, .reject] : []
        case .unsupported:
            return []
        }
    }

    var allowsDecision: Bool { !actions.isEmpty }

    var headline: String {
        switch capability {
        case .available:
            return pendingCount == 1 ? "1 approval waiting" : "\(pendingCount) approvals waiting"
        case .unsupported(let reason):
            return reason.headline
        }
    }

    var detail: String {
        switch capability {
        case .available:
            return "Provider-emitted requests. Each decision is sent to the Host and requires explicit confirmation."
        case .unsupported(let reason):
            return reason.detail
        }
    }
}

extension SessionCapabilities {
    func approvalPanel(pendingCount: Int) -> ApprovalPanelState {
        guard structuredApprovals else {
            return ApprovalPanelState(capability: .unsupported(.noNegotiatedHostCapability))
        }
        guard pendingCount > 0 else {
            return ApprovalPanelState(capability: .unsupported(.noPendingProviderApproval))
        }
        return ApprovalPanelState(capability: .available, pendingCount: pendingCount)
    }

    func composerState(isLive: Bool) -> ComposerState {
        guard isLive else {
            return ComposerState(isEnabled: false, note: ComposerState.offlineNote)
        }
        guard agentSend else {
            return ComposerState(isEnabled: false, note: ComposerState.noGrantNote)
        }
        return ComposerState(isEnabled: true, note: ComposerState.liveNote)
    }
}

/// The chat composer is enabled only while an authenticated session is live and
/// this device holds `agent.send`.
struct ComposerState: Equatable, Sendable {
    let isEnabled: Bool
    let note: String

    static let offlineNote = "Sending is disabled: no authenticated Host connection. Typed text never leaves this device."
    static let noGrantNote = "Sending is disabled: this device has no agent.send grant."
    static let liveNote = "Prompts are sent to the Host over the encrypted channel."
}

/// Fixture-only reserved region for the Remote Screen plugin. Live sessions use
/// the real Remote Screen tile instead.
enum SessionOverlayState: Equatable, Sendable {
    case reservedUnavailable(reason: String)

    static let i1Reserved = SessionOverlayState.reservedUnavailable(
        reason: "No Host is paired, so no screen capture or media path exists."
    )

    var title: String { "Remote Screen" }

    var isInteractive: Bool { false }

    var detail: String {
        switch self {
        case .reservedUnavailable(let reason):
            return reason
        }
    }
}
