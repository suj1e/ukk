import Foundation

/// Capabilities negotiated with a Host. Every flag is false in this build: no
/// Host session is authenticated and no remote command channel exists yet.
struct SessionCapabilities: Equatable, Sendable {
    let structuredApprovals: Bool
    let agentSend: Bool
    let terminalAttach: Bool
    let screenPreview: Bool
    let screenControl: Bool

    /// The only value this build can truthfully produce.
    static let unnegotiated = SessionCapabilities(
        structuredApprovals: false,
        agentSend: false,
        terminalAttach: false,
        screenPreview: false,
        screenControl: false
    )

    static let fixture = SessionCapabilities.unnegotiated
}

enum ApprovalUnsupportedReason: Equatable, Sendable {
    /// The Host has not advertised a structured approval capability.
    case noNegotiatedHostCapability
    /// A capability exists, but this build has no verified approval round-trip.
    case noVerifiedAdapterChannel

    var headline: String { "Approvals unavailable" }

    var detail: String {
        switch self {
        case .noNegotiatedHostCapability:
            return "No structured approval capability has been negotiated with a Host. Terminal text is never parsed into approvals."
        case .noVerifiedAdapterChannel:
            return "This build has no verified structured approval channel, so no decision can be sent or recorded."
        }
    }
}

/// Approvals are deliberately modelled with a single unsupported case. There is
/// no code path in this build that can present or send an approval decision.
enum ApprovalCapability: Equatable, Sendable {
    case unsupported(ApprovalUnsupportedReason)
}

enum ApprovalAction: String, Equatable, Sendable {
    case approve
    case reject
}

struct ApprovalPanelState: Equatable, Sendable {
    let capability: ApprovalCapability

    /// Always empty: no approvals can be decided from this build.
    var actions: [ApprovalAction] {
        switch capability {
        case .unsupported:
            return []
        }
    }

    var allowsDecision: Bool { !actions.isEmpty }

    var headline: String {
        switch capability {
        case .unsupported(let reason):
            return reason.headline
        }
    }

    var detail: String {
        switch capability {
        case .unsupported(let reason):
            return reason.detail
        }
    }
}

extension SessionCapabilities {
    var approvalPanel: ApprovalPanelState {
        if structuredApprovals {
            return ApprovalPanelState(capability: .unsupported(.noVerifiedAdapterChannel))
        }
        return ApprovalPanelState(capability: .unsupported(.noNegotiatedHostCapability))
    }

    var composerState: ComposerState {
        ComposerState(
            isEnabled: false,
            note: ComposerState.readOnlyNote
        )
    }
}

/// The chat composer is read-only until an authenticated Host channel can carry
/// `agent.send` with an idempotency key.
struct ComposerState: Equatable, Sendable {
    let isEnabled: Bool
    let note: String

    static let readOnlyNote = "Sending is disabled: no authenticated Host connection. agent.send ships with S1/S2; typing here never leaves this device."
}

/// A reserved region in the Session screen for the future Remote Screen plugin
/// (M1). No capture, media or control path exists in this build.
enum SessionOverlayState: Equatable, Sendable {
    case reservedUnavailable(reason: String)

    static let i1Reserved = SessionOverlayState.reservedUnavailable(
        reason: "Reserved slot for the M1 Remote Screen plugin. This build has no capture, media or pointer path."
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
