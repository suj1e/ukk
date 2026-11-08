import Foundation
import SwiftUI

/// Drives one pairing attempt. The invite secret lives only inside the
/// handshake object for the lifetime of the attempt; it is never stored,
/// logged, or rendered.
@MainActor
final class PairingCoordinator: ObservableObject {
    enum State: Equatable {
        case idle
        case connecting
        case awaitingConfirmation(code: String, hostID: String)
        case finishing
        case paired(HostTrustRecord)
        case failed(String)
        case cancelled
    }

    @Published private(set) var state: State = .idle

    /// Called on the main actor after trust is durably stored.
    var onPaired: ((HostTrustRecord) -> Void)?

    private let vault: IdentityVault
    private var pending: Pending?
    private var task: Task<Void, Never>?

    private struct Pending {
        let relay: RelayClient
        let handshake: PairingHandshake
        let iterator: AsyncStream<RelayEvent>.Iterator
        let approval: PairingApproval
        let identity: IdentityRecord
    }

    init(vault: IdentityVault) {
        self.vault = vault
    }

    var isBusy: Bool {
        switch state {
        case .connecting, .finishing: return true
        default: return false
        }
    }

    /// Parses a scanned/pasted invite and begins the handshake. The six-digit
    /// code is displayed, but nothing is trusted until `confirm()`.
    func begin(payload: String, now: Date = Date()) {
        guard !isBusy, pending == nil else { return }
        state = .connecting

        task?.cancel()
        task = Task { [weak self] in
            await self?.run(payload: payload, now: now)
        }
    }

    /// User confirmed that the Desktop shows the same six-digit code.
    func confirm() {
        guard case .awaitingConfirmation = state, let pending else { return }
        state = .finishing
        self.pending = nil

        task?.cancel()
        task = Task { [weak self] in
            await self?.finish(pending)
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
        if let pending {
            let relay = pending.relay
            Task { await relay.close() }
        }
        pending = nil
        state = .cancelled
    }

    // MARK: - Private

    private func run(payload: String, now: Date) async {
        let parsed: PairingInvite
        switch PairingInviteParser.parse(payload: payload, now: now) {
        case .success(let invite):
            parsed = invite
        case .failure(let error):
            state = .failed(error.message)
            return
        }

        do {
            let identity = try vault.loadOrCreateIdentity()
            let deviceIdentity = try identity.identity
            let handshake = try PairingHandshake(invite: parsed, identity: deviceIdentity)
            let relay = try RelayClient(
                relayURL: parsed.relay,
                hostID: parsed.hostID,
                deviceID: identity.deviceID
            )
            var iterator = relay.events.makeAsyncIterator()
            try await relay.open()
            try await relay.send(payload: handshake.c1Message)

            guard let response = try await nextData(&iterator) else {
                await relay.close()
                state = .failed("The Host did not answer the pairing handshake.")
                return
            }

            let approval = try handshake.accept(hostMessage: response)
            pending = Pending(
                relay: relay,
                handshake: handshake,
                iterator: iterator,
                approval: approval,
                identity: identity
            )
            state = .awaitingConfirmation(code: approval.code, hostID: parsed.hostID)
        } catch {
            state = .failed(message(for: error))
        }
    }

    private func finish(_ pending: Pending) async {
        do {
            try await pending.relay.send(payload: pending.handshake.c2Message)
        } catch {
            await pending.relay.close()
            state = .failed(message(for: error))
            return
        }

        let relay = pending.relay
        let channel = pending.approval.channel
        let api = RemoteAPI(
            sender: { data in
                let envelope = try channel.seal(plaintext: data)
                try await relay.send(payload: envelope)
            },
            onEvent: { _ in }
        )
        var iterator = pending.iterator
        let pump = Task {
            while let event = await iterator.next() {
                switch event {
                case .data(let envelope):
                    if let opened = try? channel.open(envelope), opened.stream == 0 {
                        await api.ingest(opened.plaintext)
                    }
                case .closed, .failed:
                    await api.close(error: .transport("relay closed"))
                    return
                case .opened:
                    break
                }
            }
            await api.close(error: .transport("relay stream ended"))
        }

        do {
            _ = try await api.request(.health, timeout: 12)
        } catch {
            pump.cancel()
            await relay.close()
            state = .failed(message(for: error))
            return
        }

        pump.cancel()
        await relay.close()

        let trust = HostTrustRecord(
            hostID: pending.approval.hostIdentity.hostID,
            hostEd: pending.approval.hostIdentity.hostEd,
            hostX: pending.approval.hostIdentity.hostX,
            relayURL: pending.approval.hostIdentity.relayURL,
            deviceID: pending.identity.deviceID,
            label: "Desktop \(pending.approval.hostIdentity.hostID.prefix(8))",
            pairedAtMS: Int64(Date().timeIntervalSince1970 * 1000)
        )
        do {
            try vault.saveTrust(trust)
        } catch {
            state = .failed(message(for: error))
            return
        }
        state = .paired(trust)
        onPaired?(trust)
    }

    private func nextData(_ iterator: inout AsyncStream<RelayEvent>.AsyncIterator) async throws -> Data? {
        while let event = await iterator.next() {
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

    private func message(for error: Error) -> String {
        switch error {
        case let error as PairingInviteParseError: return error.message
        case let error as SecureChannelError: return error.message
        case let error as RelayError: return error.message
        case let error as RemoteAPIError: return error.message
        case let error as SecretStoreError: return error.message
        default: return "Pairing failed."
        }
    }
}
