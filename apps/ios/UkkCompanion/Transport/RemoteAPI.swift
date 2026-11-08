import Foundation

enum RemoteAPIError: Error, Equatable, Sendable {
    case notConnected
    case transport(String)
    case timeout
    case malformedResponse
    case server(code: String, message: String)
    case revoked
    case pairingRequired
    case forbidden(String)

    var code: String? {
        switch self {
        case .server(let code, _): return code
        case .revoked: return "device_revoked"
        case .pairingRequired: return "pairing_required"
        case .forbidden: return "forbidden"
        default: return nil
        }
    }

    var message: String {
        switch self {
        case .notConnected:
            return "The Host connection is not open."
        case .transport(let detail):
            return "The Host connection failed: \(detail)"
        case .timeout:
            return "The Host did not answer in time."
        case .malformedResponse:
            return "The Host sent a response this client could not read."
        case .server(_, let message):
            return message
        case .revoked:
            return "This device was revoked by the Host. Pair again to reconnect."
        case .pairingRequired:
            return "The Host no longer recognises this device. Pair again."
        case .forbidden(let detail):
            return detail.isEmpty ? "The Host denied this action for this device." : detail
        }
    }
}

/// Correlates JSON requests and responses over one encrypted channel and
/// surfaces pushed Host events. Concurrent requests are supported.
actor RemoteAPI {
    typealias Sender = @Sendable (Data) async throws -> Void

    private struct Pending {
        let continuation: CheckedContinuation<JSONValue, Error>
        let timeoutTask: Task<Void, Never>
    }

    private let sender: Sender
    private let onEvent: @Sendable (WireEvent) -> Void
    private var pending: [String: Pending] = [:]
    private var closed = false
    private var requestCounter: UInt32 = 0
    private let decoder = JSONDecoder()

    init(sender: @escaping Sender, onEvent: @escaping @Sendable (WireEvent) -> Void) {
        self.sender = sender
        self.onEvent = onEvent
    }

    var isClosed: Bool { closed }

    func request(
        _ method: WireMethod,
        params: JSONValue? = nil,
        timeout: TimeInterval = 20
    ) async throws -> JSONValue {
        guard !closed else { throw RemoteAPIError.notConnected }

        let id = nextRequestID()
        let encoded: Data
        do {
            encoded = try JSONEncoder().encode(WireRequest(id: id, method: method.rawValue, params: params))
        } catch {
            throw RemoteAPIError.malformedResponse
        }

        return try await withCheckedThrowingContinuation { continuation in
            let timeoutTask = Task { [weak self] in
                let nanoseconds = UInt64(max(0.05, timeout) * 1_000_000_000)
                try? await Task.sleep(nanoseconds: nanoseconds)
                guard !Task.isCancelled else { return }
                await self?.failRequest(id, error: .timeout, closeConnection: false)
            }
            pending[id] = Pending(continuation: continuation, timeoutTask: timeoutTask)

            Task {
                do {
                    try await self.sender(encoded)
                } catch {
                    self.failRequest(
                        id,
                        error: .transport((error as? RelayError)?.message ?? "send failed"),
                        closeConnection: false
                    )
                }
            }
        }
    }

    /// Feeds one decrypted application-plane payload.
    func ingest(_ plaintext: Data) {
        guard !closed else { return }
        guard let object = try? JSONSerialization.jsonObject(with: plaintext),
              let dictionary = object as? [String: Any] else {
            return
        }

        if dictionary["type"] as? String == "event" {
            if let event = try? decoder.decode(WireEventEnvelope.self, from: plaintext).event {
                onEvent(event)
            }
            return
        }

        guard dictionary["id"] is String else { return }
        guard let response = try? decoder.decode(WireResponse.self, from: plaintext) else {
            return
        }
        guard let entry = pending.removeValue(forKey: response.id) else { return }
        entry.timeoutTask.cancel()

        if let error = response.error {
            entry.continuation.resume(throwing: Self.map(error: error))
        } else {
            entry.continuation.resume(returning: response.result ?? .null)
        }
    }

    /// Fails every in-flight request and refuses new ones.
    func close(error: RemoteAPIError = .notConnected) {
        guard !closed else { return }
        closed = true
        let entries = pending
        pending.removeAll()
        for (_, entry) in entries {
            entry.timeoutTask.cancel()
            entry.continuation.resume(throwing: error)
        }
    }

    // MARK: - Private

    private func failRequest(_ id: String, error: RemoteAPIError, closeConnection: Bool) {
        guard let entry = pending.removeValue(forKey: id) else { return }
        entry.timeoutTask.cancel()
        entry.continuation.resume(throwing: error)
        if closeConnection { close(error: error) }
    }

    private func nextRequestID() -> String {
        requestCounter &+= 1
        var value = requestCounter.bigEndian
        let counterBytes = withUnsafeBytes(of: &value) { Data($0) }
        let random = (try? HandshakeCrypto.randomBytes(4)) ?? Data([0, 0, 0, 0])
        return (counterBytes + random).map { String(format: "%02x", $0) }.joined()
    }

    private static func map(error: WireError) -> RemoteAPIError {
        switch error.code {
        case "device_revoked":
            return .revoked
        case "unknown_device", "identity_unknown", "pairing_required", "unknown_identity":
            return .pairingRequired
        case "forbidden", "missing_scope", "grant_expired", "grant_invalid":
            return .forbidden(error.message)
        default:
            return .server(code: error.code, message: error.message)
        }
    }
}
