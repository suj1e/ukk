import Foundation

enum ConnectionState: Equatable, Sendable {
    /// No Host is configured. Fixture content is rendered locally and must never
    /// be presented as live.
    case fixtureData
    case online
    case reconnecting(attempt: Int)
    case offline(OfflineReason)
    /// The app went to the background. iOS suspends transports; a foreground
    /// return must handshake again before anything is called live.
    case suspended
    /// Terminal state. A revoked device never becomes live again in this process.
    case revoked

    enum OfflineReason: Equatable, Sendable {
        case hostUnreachable
        case transportClosed

        var label: String {
            switch self {
            case .hostUnreachable: return "Host unreachable"
            case .transportClosed: return "Connection lost"
            }
        }
    }
}

enum ConnectionEvent: Equatable, Sendable {
    case transportOpened
    case transportClosed
    case reconnectAttemptFailed
    case userRequestedRetry
    case appEnteredBackground
    case appEnteredForeground
    case deviceRevoked
}

struct ConnectionIndicator: Equatable, Sendable {
    enum Tone: Equatable, Sendable {
        case neutral
        case positive
        case warning
        case negative
    }

    let label: String
    let detail: String
    let systemImage: String
    let tone: Tone
    let isLive: Bool
}

/// Pure, deterministic connection state machine shared by UI and tests.
///
/// Background/foreground and reconnect rules are intentionally conservative:
/// only an authenticated `transportOpened` produces `.online`. A foreground
/// return from suspension requires a fresh handshake.
struct ConnectionStateMachine: Equatable, Sendable {
    private(set) var state: ConnectionState
    let maxReconnectAttempts: Int

    init(initial: ConnectionState = .fixtureData, maxReconnectAttempts: Int = 5) {
        self.state = initial
        self.maxReconnectAttempts = max(1, maxReconnectAttempts)
    }

    var isLiveClaim: Bool {
        if case .online = state { return true }
        return false
    }

    var canDispatchCommands: Bool { isLiveClaim }

    var isRetryAvailable: Bool {
        if case .offline = state { return true }
        return false
    }

    mutating func handle(_ event: ConnectionEvent) {
        if event == .deviceRevoked {
            state = .revoked
            return
        }
        guard state != .revoked else { return }

        switch event {
        case .transportOpened:
            state = .online

        case .transportClosed:
            if state == .online {
                state = .reconnecting(attempt: 1)
            }

        case .reconnectAttemptFailed:
            if case .reconnecting(let attempt) = state {
                if attempt < maxReconnectAttempts {
                    state = .reconnecting(attempt: attempt + 1)
                } else {
                    state = .offline(.hostUnreachable)
                }
            }

        case .userRequestedRetry:
            switch state {
            case .offline, .fixtureData:
                state = .reconnecting(attempt: 1)
            default:
                break
            }

        case .appEnteredBackground:
            state = .suspended

        case .appEnteredForeground:
            if state == .suspended {
                state = .reconnecting(attempt: 1)
            }

        case .deviceRevoked:
            // Handled above; kept for exhaustiveness.
            break
        }
    }

    var indicator: ConnectionIndicator {
        switch state {
        case .fixtureData:
            return ConnectionIndicator(
                label: "Fixture data · no Host",
                detail: "Sample content only. Nothing on this device talks to a Host.",
                systemImage: "testtube.2",
                tone: .neutral,
                isLive: false
            )
        case .online:
            return ConnectionIndicator(
                label: "Live",
                detail: "Authenticated Host transport is open.",
                systemImage: "bolt.horizontal.circle.fill",
                tone: .positive,
                isLive: true
            )
        case .reconnecting(let attempt):
            return ConnectionIndicator(
                label: "Reconnecting (attempt \(attempt))",
                detail: "Negotiating an authenticated session. Content stays read-only.",
                systemImage: "arrow.triangle.2.circlepath",
                tone: .warning,
                isLive: false
            )
        case .offline(let reason):
            return ConnectionIndicator(
                label: reason.label,
                detail: "Read-only cached view. Commands and approvals are disabled.",
                systemImage: "wifi.slash",
                tone: .negative,
                isLive: false
            )
        case .suspended:
            return ConnectionIndicator(
                label: "Paused in background",
                detail: "iOS suspends network transports. A foreground return reconnects and re-authenticates.",
                systemImage: "moon.zzz",
                tone: .neutral,
                isLive: false
            )
        case .revoked:
            return ConnectionIndicator(
                label: "Device revoked",
                detail: "Trust was revoked. Re-pairing requires explicit Host confirmation.",
                systemImage: "lock.slash",
                tone: .negative,
                isLive: false
            )
        }
    }

    /// Bounded, deterministic reconnect backoff in seconds.
    static func retryDelay(forAttempt attempt: Int) -> TimeInterval {
        let boundedAttempt = max(1, min(attempt, 6))
        let seconds = pow(2.0, Double(boundedAttempt - 1))
        return min(30, seconds)
    }
}
