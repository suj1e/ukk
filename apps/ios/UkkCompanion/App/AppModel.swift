import Foundation
import SwiftUI

/// App-level state. In fixture mode (no pinned Host) it renders deterministic
/// sample content. Once a Host is paired, every workspace, Session, message,
/// tool, approval and device is loaded through the encrypted RPC channel and
/// marked stale whenever the connection is not live.
@MainActor
final class AppModel: ObservableObject {
    let catalog: FixtureCatalog
    let vault: IdentityVault
    private let defaults: UserDefaults

    @Published var navigation = NavigationState()
    @Published private(set) var connection = ConnectionStateMachine()
    @Published private(set) var trust: HostTrustRecord?
    @Published private(set) var liveWorkspace: Workspace?
    @Published private(set) var liveSnapshot: WorkspaceSnapshot?
    @Published private(set) var remoteStatus: RemoteStatus?
    @Published private(set) var devices: [TrustedDevice] = []
    @Published private(set) var agentSnapshots: [String: AgentSnapshot] = [:]
    @Published private(set) var lastError: String?
    @Published private(set) var sessionError: String?
    @Published private(set) var isRefreshing = false
    @Published private(set) var busySessionID: String?
    @Published var isShowingTrustedDevices = false
    let pairing: PairingCoordinator

    private(set) var client: RemoteHostClient?
    private var statusTask: Task<Void, Never>?
    private var eventTask: Task<Void, Never>?
    private var didStart = false
    private static let lastSessionKey = "dev.ukk.companion.last-session"

    init(
        catalog: FixtureCatalog = .fixture,
        connection: ConnectionStateMachine = ConnectionStateMachine(),
        vault: IdentityVault? = nil,
        defaults: UserDefaults = .standard
    ) {
        let resolvedVault = vault ?? IdentityVault.live()
        self.catalog = catalog
        self.connection = connection
        self.vault = resolvedVault
        self.defaults = defaults
        self.pairing = PairingCoordinator(vault: resolvedVault)

        // UI tests and previews can force the honest no-Host mode.
        if !ProcessInfo.processInfo.arguments.contains("-ukk-fixture"),
           let loaded = try? resolvedVault.loadTrust() {
            trust = loaded
            self.connection = ConnectionStateMachine(initial: .connecting)
        }
        pairing.onPaired = { [weak self] record in
            self?.adoptTrust(record)
        }
    }

    // MARK: - Derived content

    var isFixtureMode: Bool { trust == nil }

    var isDataLive: Bool { connection.isLiveClaim }

    var workspaces: [Workspace] {
        if isFixtureMode { return catalog.workspaces }
        return liveWorkspace.map { [$0] } ?? []
    }

    func workspace(_ id: WorkspaceID) -> Workspace? {
        workspaces.first { $0.id == id }
    }

    func space(_ id: SpaceID) -> Space? {
        for workspace in workspaces {
            if let space = workspace.space(id) { return space }
        }
        return nil
    }

    func session(_ id: SessionID) -> Session? {
        for workspace in workspaces {
            for space in workspace.spaces {
                if let session = space.session(id) { return session }
            }
        }
        return nil
    }

    func agentRun(_ id: AgentRunID) -> AgentRun? {
        for workspace in workspaces {
            for space in workspace.spaces {
                for session in space.sessions {
                    if let agent = session.agentRun(id) { return agent }
                }
            }
        }
        return nil
    }

    func rpcID(for session: Session) -> String? {
        if let rpcID = session.rpcID { return rpcID }
        return liveSnapshot?.sessions.first { SessionID.fromHost($0.id) == session.id }?.id
    }

    // MARK: - Lifecycle

    func start() {
        guard !didStart else { return }
        didStart = true
        if trust != nil {
            startClient()
        }
    }

    func handleScenePhase(_ phase: ScenePhase) {
        switch phase {
        case .background:
            connection.handle(.appEnteredBackground)
            if trust != nil { stopClient() }
        case .active:
            guard trust != nil else {
                connection.handle(.appEnteredForeground)
                return
            }
            if case .suspended = connection.state {
                connection.handle(.appEnteredForeground)
                startClient()
            }
        case .inactive:
            break
        @unknown default:
            break
        }
    }

    func retryConnection() {
        guard trust != nil else { return }
        connection.handle(.userRequestedRetry)
        startClient()
    }

    /// Drops the pinned Host. The caller must have asked the user to confirm.
    func forgetHost() {
        stopClient()
        try? vault.clearTrust()
        defaults.removeObject(forKey: Self.lastSessionKey)
        trust = nil
        liveSnapshot = nil
        liveWorkspace = nil
        remoteStatus = nil
        devices = []
        agentSnapshots = [:]
        sessionError = nil
        lastError = nil
        connection.handle(.hostForgotten)
        navigation.popToRoot()
    }

    // MARK: - Pairing

    func beginPairing(payload: String) {
        let trimmed = payload.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            lastError = "Paste or scan a pairing invite first."
            return
        }
        if trust != nil {
            lastError = "Forget the current Host before pairing another one."
            return
        }
        lastError = nil
        pairing.begin(payload: trimmed)
    }

    func cancelPairing() {
        pairing.cancel()
    }

    private func adoptTrust(_ record: HostTrustRecord) {
        stopClient()
        trust = record
        liveSnapshot = nil
        liveWorkspace = nil
        agentSnapshots = [:]
        remoteStatus = nil
        devices = []
        connection = ConnectionStateMachine(initial: .connecting)
        startClient()
    }

    // MARK: - Connection

    private func startClient() {
        guard let trust else { return }
        stopClient()

        let identity: IdentityRecord
        do {
            identity = try vault.loadOrCreateIdentity()
        } catch {
            lastError = (error as? SecretStoreError)?.message ?? "Could not load the device identity."
            connection.handle(.wentOffline(.hostUnreachable))
            return
        }

        let client = RemoteHostClient(
            configuration: RemoteHostClient.Configuration(trust: trust, identity: identity)
        )
        self.client = client
        connection.handle(.dialStarted)

        statusTask = Task { [weak self] in
            for await status in client.statusUpdates {
                guard !Task.isCancelled else { return }
                self?.apply(status: status)
            }
        }
        eventTask = Task { [weak self] in
            for await event in client.events {
                guard !Task.isCancelled else { return }
                self?.apply(event: event)
            }
        }
        Task { await client.start() }
    }

    private func stopClient() {
        statusTask?.cancel()
        eventTask?.cancel()
        statusTask = nil
        eventTask = nil
        guard let client else { return }
        self.client = nil
        Task { await client.stop() }
    }

    private func apply(status: RemoteHostClient.Status) {
        switch status {
        case .idle:
            break
        case .connecting:
            connection.handle(.dialStarted)
        case .online:
            connection.handle(.transportOpened)
            Task { await self.refreshAll() }
        case .degraded(let detail):
            connection.handle(.connectionDegraded(detail))
        case .reconnecting(let attempt):
            connection.handle(.reconnectScheduled(attempt))
        case .offline(let reason):
            connection.handle(.wentOffline(.hostUnreachable))
            lastError = reason
        case .revoked:
            connection.handle(.deviceRevoked)
            lastError = "This device was revoked by the Host. Pair again to reconnect."
        case .pairingRequired:
            connection.handle(.pairingRequired)
            lastError = "The Host no longer recognises this device. Pair again."
        }
    }

    private func apply(event: WireEvent) {
        if let error = event.error, !error.isEmpty {
            if error.lowercased().contains("revoked") {
                connection.handle(.deviceRevoked)
                lastError = "This device was revoked by the Host. Pair again to reconnect."
            } else {
                lastError = error
            }
            return
        }
        guard let agent = event.agent else { return }
        let key = event.sessionID ?? agent.sessionID
        guard let key else { return }
        agentSnapshots[key] = agent
        if connection.isLiveClaim {
            rebuildLiveWorkspace()
        }
    }

    // MARK: - Refresh

    func refreshAll() async {
        guard connection.isLiveClaim, let client else { return }
        isRefreshing = true
        defer { isRefreshing = false }

        do {
            let snapshot = try await client.workspace()
            liveSnapshot = snapshot

            if let status = try? await client.remoteStatus() {
                apply(remoteStatus: status)
            }

            for record in snapshot.sessions {
                if let agent = try? await client.agentSnapshot(sessionID: record.id) {
                    agentSnapshots[record.id] = agent
                }
            }

            connection.handle(.dataFresh)
            rebuildLiveWorkspace()
            restoreLastSessionIfNeeded()
            sessionError = nil
        } catch {
            handle(error)
        }
    }

    func refreshSession(_ rpcID: String) async {
        guard connection.isLiveClaim, let client else { return }
        busySessionID = rpcID
        defer { busySessionID = nil }
        do {
            agentSnapshots[rpcID] = try await client.agentSnapshot(sessionID: rpcID)
            rebuildLiveWorkspace()
            sessionError = nil
        } catch {
            sessionError = (error as? RemoteAPIError)?.message ?? "Could not load this Session."
        }
    }

    private func apply(remoteStatus status: RemoteStatus) {
        remoteStatus = status
        devices = status.devices.map {
            TrustedDevice(remote: $0, currentDeviceID: trust?.deviceID ?? "")
        }
        if let current = status.devices.first(where: { $0.id == trust?.deviceID }), current.revoked {
            connection.handle(.deviceRevoked)
            lastError = "This device was revoked by the Host. Pair again to reconnect."
        }
        rebuildLiveWorkspace()
    }

    func loadDevices() async {
        guard let client, connection.isLiveClaim else {
            devices = []
            return
        }
        do {
            apply(remoteStatus: try await client.remoteStatus())
        } catch {
            handle(error)
        }
    }

    // MARK: - Commands

    func sendPrompt(session: Session, text: String) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let rpcID = rpcID(for: session) else { return false }
        guard connection.canDispatchCommands, let client else {
            lastError = "Sending is disabled while the Host connection is not live."
            return false
        }
        busySessionID = rpcID
        defer { busySessionID = nil }
        do {
            try await client.prompt(sessionID: rpcID, text: trimmed)
            if let agent = try? await client.agentSnapshot(sessionID: rpcID) {
                agentSnapshots[rpcID] = agent
                rebuildLiveWorkspace()
            }
            return true
        } catch {
            handle(error)
            return false
        }
    }

    func abort(session: Session) async {
        await runSessionCommand(session) { client, rpcID in
            try await client.abort(sessionID: rpcID)
        }
    }

    func stopAgent(session: Session) async {
        await runSessionCommand(session) { client, rpcID in
            try await client.stopAgent(sessionID: rpcID)
        }
    }

    func respond(to approval: ApprovalRequest, session: Session, approve: Bool) async {
        guard let rpcID = rpcID(for: session), connection.canDispatchCommands, let client else {
            lastError = "Approvals are disabled while the Host connection is not live."
            return
        }
        busySessionID = rpcID
        defer { busySessionID = nil }
        do {
            try await client.respondToApproval(
                sessionID: rpcID,
                approvalID: approval.id,
                approve: approve,
                note: nil
            )
            if let agent = try? await client.agentSnapshot(sessionID: rpcID) {
                agentSnapshots[rpcID] = agent
                rebuildLiveWorkspace()
            }
        } catch {
            handle(error)
        }
    }

    private func runSessionCommand(
        _ session: Session,
        _ operation: (RemoteHostClient, String) async throws -> Void
    ) async {
        guard let rpcID = rpcID(for: session), connection.canDispatchCommands, let client else {
            lastError = "Commands are disabled while the Host connection is not live."
            return
        }
        busySessionID = rpcID
        defer { busySessionID = nil }
        do {
            try await operation(client, rpcID)
            await refreshSession(rpcID)
        } catch {
            handle(error)
        }
    }

    func revokeDevice(_ device: TrustedDevice) async {
        guard let client, connection.isLiveClaim else {
            lastError = "Revocation requires a live Host connection."
            return
        }
        do {
            try await client.revoke(deviceID: device.remoteID)
            if device.isCurrentDevice {
                stopClient()
                try? vault.clearTrust()
                defaults.removeObject(forKey: Self.lastSessionKey)
                trust = nil
                liveSnapshot = nil
                liveWorkspace = nil
                devices = []
                agentSnapshots = [:]
                connection.handle(.deviceRevoked)
                navigation.popToRoot()
                lastError = "This device revoked itself. Pair again to reconnect."
            } else {
                await loadDevices()
            }
        } catch {
            handle(error)
        }
    }

    // MARK: - Persistence (host + last session only)

    func noteSessionOpened(_ session: Session) {
        guard let rpcID = rpcID(for: session) else { return }
        defaults.set(rpcID, forKey: Self.lastSessionKey)
    }

    private func restoreLastSessionIfNeeded() {
        guard navigation.path.isEmpty,
              let rpcID = defaults.string(forKey: Self.lastSessionKey),
              let snapshot = liveSnapshot,
              snapshot.sessions.contains(where: { $0.id == rpcID }),
              let live = liveWorkspace else {
            return
        }
        guard let space = live.spaces.first(where: {
            $0.sessions.contains(where: { $0.rpcID == rpcID })
        }), let session = space.sessions.first(where: { $0.rpcID == rpcID }) else {
            return
        }
        navigation.path = [
            .workspace(live.id),
            .space(space.id),
            .session(session.id)
        ]
    }

    // MARK: - Errors

    private func handle(_ error: Error) {
        let apiError = error as? RemoteAPIError
        if case .revoked? = apiError {
            connection.handle(.deviceRevoked)
            lastError = apiError?.message
            return
        }
        if case .pairingRequired? = apiError {
            connection.handle(.pairingRequired)
            lastError = apiError?.message
            return
        }
        if case .notConnected? = apiError {
            connection.handle(.connectionDegraded("The Host connection closed."))
        }
        lastError = message(for: error)
    }

    private func message(for error: Error) -> String {
        switch error {
        case let error as RemoteAPIError: return error.message
        case let error as SecureChannelError: return error.message
        case let error as RelayError: return error.message
        case let error as SecretStoreError: return error.message
        default: return "The Host request failed."
        }
    }

    // MARK: - Live projection

    private var capabilities: SessionCapabilities {
        guard let status = remoteStatus,
              let deviceID = trust?.deviceID,
              let device = status.devices.first(where: { $0.id == deviceID }) else {
            return .unnegotiated
        }
        let scopes = Set(device.scopes.compactMap(DeviceScope.init(rawValue:)))
        return SessionCapabilities(scopes: scopes)
    }

    private func rebuildLiveWorkspace() {
        guard let snapshot = liveSnapshot, let trust else {
            liveWorkspace = nil
            return
        }
        let workspaceID = WorkspaceID.fromHost("workspace:\(trust.hostID)")
        let caps = capabilities

        var spaces: [Space] = []
        for spaceRecord in snapshot.spaces {
            let spaceID = SpaceID.fromHost(spaceRecord.id)
            let sessions = snapshot.sessions
                .filter { $0.spaceID == spaceRecord.id }
                .map { buildSession($0, spaceID: spaceID, capabilities: caps) }
            spaces.append(
                Space(
                    id: spaceID,
                    name: spaceRecord.title,
                    projectRoot: spaceRecord.projectRoot,
                    sessions: sessions,
                    provenance: .live
                )
            )
        }

        let workspace = Workspace(
            id: workspaceID,
            name: trust.label,
            hostLabel: "Paired Desktop · \(trust.hostID.prefix(8))",
            spaces: spaces,
            provenance: .live
        )
        liveWorkspace = workspace
    }

    private func buildSession(
        _ record: SessionRecord,
        spaceID: SpaceID,
        capabilities: SessionCapabilities
    ) -> Session {
        let sessionID = SessionID.fromHost(record.id)
        let snapshot = agentSnapshots[record.id]
        let records = (liveSnapshot?.agents ?? []).filter { $0.sessionID == record.id }

        var agents: [AgentRun] = records.map { agent in
            let providerState = snapshot?.state
            let status = providerState.map(AgentRunStatus.init(providerState:)) ?? AgentRunStatus(rawValue: agent.status) ?? .unknown
            return AgentRun(
                id: AgentRunID.fromHost(agent.id),
                sessionID: sessionID,
                name: agent.adapter.isEmpty ? "Agent" : agent.adapter,
                adapterKind: agent.adapter,
                status: status,
                statusSource: providerState == nil
                    ? "Host workspace record"
                    : "provider snapshot (\(snapshot?.provider ?? "unknown"))",
                terminalRef: TerminalRef(machine: agent.coreID, terminal: agent.terminalAddress),
                provenance: .live
            )
        }

        if agents.isEmpty, let snapshot {
            agents = [
                AgentRun(
                    id: AgentRunID.fromHost("agent:\(record.id)"),
                    sessionID: sessionID,
                    name: snapshot.provider,
                    adapterKind: snapshot.provider,
                    status: AgentRunStatus(providerState: snapshot.state),
                    statusSource: "provider snapshot (\(snapshot.provider))",
                    terminalRef: TerminalRef(machine: "host", terminal: "unknown"),
                    provenance: .live
                )
            ]
        }

        let primaryAgentID = agents.first?.id
        let messages = (snapshot?.messages ?? []).map { message in
            ChatMessage(
                id: ChatMessageID.fromHost("\(record.id):\(message.id)"),
                sessionID: sessionID,
                agentRunID: primaryAgentID,
                role: ChatRole(wireRole: message.role),
                text: message.text,
                isFinal: message.isFinal,
                occurredAt: Date(timeIntervalSince1970: Double(message.createdMS) / 1000),
                provenance: .live
            )
        }
        let tools = (snapshot?.tools ?? []).map { tool in
            ToolCall(
                id: tool.id,
                name: tool.name,
                status: tool.status,
                input: Self.summarize(tool.input),
                output: Self.summarize(tool.output),
                isError: tool.isError
            )
        }
        let approvals = (snapshot?.pendingApprovals ?? []).map { approval in
            ApprovalRequest(
                id: approval.id,
                toolName: approval.toolName,
                input: Self.summarize(approval.input),
                state: approval.state,
                expiresAt: approval.expiresMS.map { Date(timeIntervalSince1970: Double($0) / 1000) }
            )
        }

        return Session(
            id: sessionID,
            spaceID: spaceID,
            title: record.title,
            lifecycle: SessionLifecycle(rawValue: record.lifecycle) ?? .active,
            agentRuns: agents,
            messages: messages,
            tools: tools,
            approvals: approvals,
            capabilities: capabilities,
            rpcID: record.id,
            provenance: .live
        )
    }

    private static func summarize(_ value: JSONValue?) -> String? {
        guard let value else { return nil }
        if case .string(let text) = value { return text }
        guard let data = try? JSONEncoder().encode(value),
              let text = String(data: data, encoding: .utf8) else {
            return nil
        }
        return text.count > 600 ? String(text.prefix(600)) + "…" : text
    }
}
