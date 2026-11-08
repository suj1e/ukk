import Foundation

enum ConnectionState: Equatable, Sendable {
    /// No Host is configured. Sample content is rendered locally and must never
    /// be presented as live.
    case fixtureData
    /// A pinned Host exists and the client is dialling or resuming.
    case connecting
    case online
    /// The transport is still open but the last data is stale or a channel-level
    /// failure occurred. Cached content must not be labelled live.
    case degraded(String)
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
        /// The Host no longer recognises this device. Re-pairing is required.
        case pairingRequired

        var label: String {
            switch self {
            case .hostUnreachable: return "Host unreachable"
            case .transportClosed: return "Connection lost"
            case .pairingRequired: return "Pairing required"
            }
        }

        var detail: String {
            switch self {
            case .hostUnreachable:
                return "The Host did not answer. Cached content is stale; commands and approvals are disabled."
            case .transportClosed:
                return "The encrypted session ended. Cached content is stale; reconnect to continue."
            case .pairingRequired:
                return "The Host no longer recognises this device. Pair again with a fresh invite."
            }
        }
    }
}

enum ConnectionEvent: Equatable, Sendable {
    case dialStarted
    case transportOpened
    case transportClosed
    case connectionDegraded(String)
    case dataFresh
    case reconnectAttemptFailed
    case reconnectScheduled(Int)
    case wentOffline(ConnectionState.OfflineReason)
    case pairingRequired
    case userRequestedRetry
    case appEnteredBackground
    case appEnteredForeground
    case deviceRevoked
    case hostForgotten
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
/// Only a completed authenticated resume produces `.online`. Stale data after a
/// channel failure is `.degraded`, never "Live". Background/foreground remains
/// conservative: a foreground return requires a fresh resume handshake.
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

    var isFixtureMode: Bool {
        if case .fixtureData = state { return true }
        return false
    }

    var isConfigured: Bool { !isFixtureMode }

    mutating func handle(_ event: ConnectionEvent) {
        if event == .deviceRevoked {
            state = .revoked
            return
        }
        if event == .hostForgotten {
            state = .fixtureData
            return
        }
        guard state != .revoked else { return }

        switch event {
        case .dialStarted:
            state = .connecting

        case .transportOpened:
            // While suspended the OS has paused transports. A late open must not
            // bypass the foreground handshake.
            if state != .suspended {
                state = .online
            }

        case .transportClosed:
            switch state {
            case .online, .connecting, .degraded(_):
                state = .reconnecting(attempt: 1)
            default:
                break
            }

        case .connectionDegraded(let detail):
            switch state {
            case .online, .connecting:
                state = .degraded(detail)
            default:
                break
            }

        case .dataFresh:
            if case .degraded(_) = state {
                state = .online
            }

        case .reconnectScheduled(let attempt):
            state = .reconnecting(attempt: max(1, attempt))

        case .wentOffline(let reason):
            state = .offline(reason)

        case .reconnectAttemptFailed:
            switch state {
            case .reconnecting(let attempt):
                if attempt < maxReconnectAttempts {
                    state = .reconnecting(attempt: attempt + 1)
                } else {
                    state = .offline(.hostUnreachable)
                }
            case .connecting:
                state = .offline(.hostUnreachable)
            default:
                break
            }

        case .pairingRequired:
            state = .offline(.pairingRequired)

        case .userRequestedRetry:
            switch state {
            case .offline, .fixtureData:
                state = .connecting
            default:
                break
            }

        case .appEnteredBackground:
            switch state {
            case .online, .connecting, .reconnecting, .degraded(_):
                state = .suspended
            default:
                break
            }

        case .appEnteredForeground:
            if state == .suspended {
                state = .connecting
            }

        case .deviceRevoked, .hostForgotten:
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
        case .connecting:
            return ConnectionIndicator(
                label: "Connecting",
                detail: "Resuming the encrypted session with the pinned Host.",
                systemImage: "arrow.triangle.2.circlepath",
                tone: .warning,
                isLive: false
            )
        case .online:
            return ConnectionIndicator(
                label: "Live",
                detail: "Authenticated Host session is open.",
                systemImage: "bolt.horizontal.circle.fill",
                tone: .positive,
                isLive: true
            )
        case .degraded(let detail):
            return ConnectionIndicator(
                label: "Degraded",
                detail: detail,
                systemImage: "exclamationmark.arrow.triangle.2.circlepath",
                tone: .warning,
                isLive: false
            )
        case .reconnecting(let attempt):
            return ConnectionIndicator(
                label: "Reconnecting (attempt \(attempt))",
                detail: "Negotiating a fresh resume handshake. Cached content is stale.",
                systemImage: "arrow.triangle.2.circlepath",
                tone: .warning,
                isLive: false
            )
        case .offline(let reason):
            return ConnectionIndicator(
                label: reason.label,
                detail: reason.detail,
                systemImage: "wifi.slash",
                tone: .negative,
                isLive: false
            )
        case .suspended:
            return ConnectionIndicator(
                label: "Paused in background",
                detail: "iOS suspends network transports. A foreground return resumes and re-authenticates.",
                systemImage: "moon.zzz",
                tone: .neutral,
                isLive: false
            )
        case .revoked:
            return ConnectionIndicator(
                label: "Device revoked",
                detail: "This device's trust was revoked. Pair again to reconnect.",
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
