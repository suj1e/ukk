import Foundation

/// Live Host connection: relay socket, device-authenticated resume handshake,
/// encrypted channel, RPC correlation and bounded reconnect.
///
/// The client never persists channel keys or counters. Every (re)connection
/// performs a fresh resume handshake with fresh ephemerals, so a relay drop or
/// an app relaunch never reuses key material.
actor RemoteHostClient {
    enum Status: Equatable, Sendable {
        case idle
        case connecting
        case online
        case degraded(String)
        case reconnecting(attempt: Int)
        case offline(String)
        case revoked
        case pairingRequired
    }

    struct Configuration {
        let trust: HostTrustRecord
        let identity: IdentityRecord
        let maxReconnectAttempts: Int

        init(trust: HostTrustRecord, identity: IdentityRecord, maxReconnectAttempts: Int = 5) {
            self.trust = trust
            self.identity = identity
            self.maxReconnectAttempts = max(1, maxReconnectAttempts)
        }
    }

    private let configuration: Configuration
    private let statusContinuation: AsyncStream<Status>.Continuation
    private let eventContinuation: AsyncStream<WireEvent>.Continuation
    private var runTask: Task<Void, Never>?
    private var currentRelay: RelayClient?
    private var currentAPI: RemoteAPI?
    private var stopped = false
    private var attempt = 0

    nonisolated let statusUpdates: AsyncStream<Status>
    nonisolated let events: AsyncStream<WireEvent>

    init(configuration: Configuration) {
        self.configuration = configuration

        var statusContinuation: AsyncStream<Status>.Continuation!
        self.statusUpdates = AsyncStream(bufferingPolicy: .bufferingNewest(8)) { statusContinuation = $0 }
        self.statusContinuation = statusContinuation

        var eventContinuation: AsyncStream<WireEvent>.Continuation!
        self.events = AsyncStream(bufferingPolicy: .bufferingNewest(64)) { eventContinuation = $0 }
        self.eventContinuation = eventContinuation
    }

    var hostID: String { configuration.trust.hostID }
    var deviceID: String { configuration.identity.deviceID }

    func start() {
        guard runTask == nil, !stopped else { return }
        runTask = Task { [weak self] in
            await self?.run()
        }
    }

    func stop() async {
        stopped = true
        runTask?.cancel()
        if let relay = currentRelay {
            await relay.close()
        }
        if let api = currentAPI {
            await api.close(error: .notConnected)
        }
        currentRelay = nil
        currentAPI = nil
        statusContinuation.yield(.idle)
        statusContinuation.finish()
        eventContinuation.finish()
    }

    // MARK: - RPC surface

    func request(_ method: WireMethod, params: JSONValue? = nil, timeout: TimeInterval = 20) async throws -> JSONValue {
        guard let api = currentAPI else { throw RemoteAPIError.notConnected }
        return try await api.request(method, params: params, timeout: timeout)
    }

    func workspace() async throws -> WorkspaceSnapshot {
        try decode(try await request(.workspaceGet), as: WorkspaceSnapshot.self)
    }

    func agentSnapshot(sessionID: String) async throws -> AgentSnapshot {
        try decode(try await request(.agentSnapshot, params: WireParams.session(sessionID)), as: AgentSnapshot.self)
    }

    func agentList() async throws -> [AgentSnapshot] {
        let value = try await request(.agentList)
        guard case .object(let object) = value,
              let sessions = object["sessions"],
              case .array(let items) = sessions else {
            throw RemoteAPIError.malformedResponse
        }
        return try items.map { try decode($0, as: AgentSnapshot.self) }
    }

    func prompt(sessionID: String, text: String) async throws {
        _ = try await request(.agentPrompt, params: WireParams.prompt(sessionID: sessionID, text: text))
    }

    func abort(sessionID: String) async throws {
        _ = try await request(.agentAbort, params: WireParams.session(sessionID))
    }

    func stopAgent(sessionID: String) async throws {
        _ = try await request(.agentStop, params: WireParams.session(sessionID))
    }

    func respondToApproval(sessionID: String, approvalID: String, approve: Bool, note: String?) async throws {
        _ = try await request(
            .agentApprove,
            params: WireParams.approval(sessionID: sessionID, approvalID: approvalID, approve: approve, note: note)
        )
    }

    func remoteStatus() async throws -> RemoteStatus {
        try decode(try await request(.remoteStatus), as: RemoteStatus.self)
    }

    func revoke(deviceID: String) async throws {
        _ = try await request(.remoteRevoke, params: WireParams.revoke(deviceID: deviceID))
    }

    func screenStart(sessionID: String, control: Bool) async throws -> ScreenSessionRecord {
        try decode(
            try await request(.screenStart, params: WireParams.screenStart(sessionID: sessionID, view: true, control: control)),
            as: ScreenSessionRecord.self
        )
    }

    func screenAnswer(sessionID: String, sdp: String) async throws {
        _ = try await request(.screenAnswer, params: WireParams.screenAnswer(sessionID: sessionID, sdp: sdp))
    }

    func screenInput(sessionID: String, events: [ScreenInputEvent]) async throws {
        _ = try await request(.screenInput, params: WireParams.screenInput(sessionID: sessionID, events: events))
    }

    func screenStop(sessionID: String) async throws {
        _ = try await request(.screenStop, params: WireParams.session(sessionID))
    }

    // MARK: - Connection lifecycle

    private func run() async {
        while !stopped {
            if attempt == 0 {
                statusContinuation.yield(.connecting)
            } else {
                statusContinuation.yield(.reconnecting(attempt: attempt))
            }

            do {
                let connection = try await openConnection()
                let pumpTask = Task { [weak self] in
                    await self?.pump(connection)
                }
                do {
                    _ = try await connection.api.request(.health, timeout: 10)
                } catch {
                    pumpTask.cancel()
                    await connection.relay.close()
                    await connection.api.close(error: .transport("handshake verification failed"))
                    throw error
                }
                attempt = 0
                statusContinuation.yield(.online)
                await pumpTask.value
            } catch let error as RemoteAPIError where error == .revoked {
                statusContinuation.yield(.revoked)
                break
            } catch let error as RemoteAPIError where error == .pairingRequired {
                statusContinuation.yield(.pairingRequired)
                break
            } catch {
                let message = describe(error)
                if let mapped = mapHandshakeFailure(message) {
                    statusContinuation.yield(mapped)
                    break
                }
                statusContinuation.yield(.degraded(message))
            }

            if let relay = currentRelay {
                await relay.close()
            }
            if let api = currentAPI {
                await api.close(error: .notConnected)
            }
            currentRelay = nil
            currentAPI = nil

            guard !stopped else { break }
            attempt += 1
            if attempt > configuration.maxReconnectAttempts {
                statusContinuation.yield(.offline("Host unreachable after \(configuration.maxReconnectAttempts) attempts."))
                break
            }
            let delay = ConnectionStateMachine.retryDelay(forAttempt: attempt)
            statusContinuation.yield(.reconnecting(attempt: attempt))
            do {
                try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            } catch {
                break
            }
        }

        if let relay = currentRelay {
            await relay.close()
        }
        if let api = currentAPI {
            await api.close(error: .notConnected)
        }
        currentRelay = nil
        currentAPI = nil
    }

    private struct Connection {
        let relay: RelayClient
        let channel: SecureChannel
        let api: RemoteAPI
        let cursor: RelayCursor
    }

    private func openConnection() async throws -> Connection {
        let trust = configuration.trust
        let identity = try configuration.identity.identity
        let relay = try RelayClient(
            relayURL: trust.relayURL,
            hostID: trust.hostID,
            deviceID: trust.deviceID
        )
        currentRelay = relay
        let cursor = RelayCursor(relay.events.makeAsyncIterator())
        try await relay.open()

        let resume = try ResumeHandshake(host: try trust.pinnedIdentity, identity: identity)
        try await relay.send(payload: resume.r1Message)

        guard let first = try await nextData(cursor) else {
            throw RelayError.closedByPeer
        }
        let approval = try resume.accept(hostMessage: first)
        let channel = approval.channel
        try await relay.send(payload: resume.r3Message)

        let api = RemoteAPI(
            sender: { data in
                let envelope = try channel.seal(plaintext: data)
                try await relay.send(payload: envelope)
            },
            onEvent: { [eventContinuation] event in
                eventContinuation.yield(event)
            }
        )
        currentAPI = api
        return Connection(relay: relay, channel: channel, api: api, cursor: cursor)
    }

    private func pump(_ connection: Connection) async {
        while !stopped {
            guard let event = await connection.cursor.next() else {
                await connection.api.close(error: .transport("relay stream ended"))
                return
            }
            switch event {
            case .data(let envelope):
                do {
                    let opened = try connection.channel.open(envelope)
                    guard opened.stream == 0 else {
                        throw SecureChannelError.envelopeStream(opened.stream)
                    }
                    await connection.api.ingest(opened.plaintext)
                } catch {
                    statusContinuation.yield(.degraded((error as? SecureChannelError)?.message ?? "channel failure"))
                    await connection.api.close(error: .transport("secure channel failure"))
                    await connection.relay.close()
                    return
                }
            case .closed(let reason):
                statusContinuation.yield(.degraded("Relay closed: \(reason)"))
                await connection.api.close(error: .transport("relay closed"))
                return
            case .failed(let reason):
                statusContinuation.yield(.degraded(reason))
                await connection.api.close(error: .transport(reason))
                return
            case .opened:
                break
            }
        }
    }

    private func nextData(_ cursor: RelayCursor) async throws -> Data? {
        while let event = await cursor.next() {
            switch event {
            case .data(let data):
                return data
            case .closed(let reason):
                throw RelayError.transport("relay closed: \(reason)")
            case .failed(let reason):
                throw RelayError.transport(reason)
            case .opened:
                continue
            }
        }
        return nil
    }

    private func describe(_ error: Error) -> String {
        switch error {
        case let error as SecureChannelError: return error.message
        case let error as RemoteAPIError: return error.message
        case let error as RelayError: return error.message
        case let error as SecretStoreError: return error.message
        default: return "Connection failed."
        }
    }

    /// Resume rejection is terminal: retrying with the same pinned identity can
    /// never succeed. Trust is cleared only after the user confirms.
    private func mapHandshakeFailure(_ message: String) -> Status? {
        let lowered = message.lowercased()
        if lowered.contains("revoked") {
            return .revoked
        }
        if lowered.contains("unknown_device") || lowered.contains("unknown device") || lowered.contains("not paired") {
            return .pairingRequired
        }
        return nil
    }

    /// Single-consumer cursor moved between the handshake phase and the pump.
    final class RelayCursor: @unchecked Sendable {
        private var iterator: AsyncStream<RelayEvent>.AsyncIterator

        init(_ iterator: AsyncStream<RelayEvent>.AsyncIterator) {
            self.iterator = iterator
        }

        func next() async -> RelayEvent? {
            await iterator.next()
        }
    }

    private func decode<T: Decodable>(_ value: JSONValue, as type: T.Type) throws -> T {
        let data = try JSONEncoder().encode(value)
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw RemoteAPIError.malformedResponse
        }
    }
}
