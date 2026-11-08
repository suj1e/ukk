import Foundation

enum RelayError: Error, Equatable, Sendable {
    case invalidEndpoint
    case notConnected
    case transport(String)
    case closedByPeer
    case protocolViolation(String)

    var message: String {
        switch self {
        case .invalidEndpoint:
            return "The relay endpoint in the invitation is not usable."
        case .notConnected:
            return "The relay connection is not open."
        case .transport(let detail):
            return "The relay connection failed: \(detail)"
        case .closedByPeer:
            return "The relay closed the connection."
        case .protocolViolation(let detail):
            return "The relay sent an unexpected frame: \(detail)"
        }
    }
}

enum RelayEvent: Sendable {
    case opened
    case data(Data)
    case closed(String)
    case failed(String)
}

/// One WebSocket connection to the ukk relay, carrying opaque encrypted
/// payloads. Reconnection policy belongs to `RemoteHostClient`; this type owns a
/// single socket lifetime and a keepalive ping.
actor RelayClient {
    private let hostID: String
    private let deviceID: String
    private let endpoint: URL
    private let session: URLSession
    private var task: URLSessionWebSocketTask?
    private var receiveTask: Task<Void, Never>?
    private var pingTask: Task<Void, Never>?
    private var connectionID: String?
    private var isClosed = false
    private let eventsContinuation: AsyncStream<RelayEvent>.Continuation

    /// Frames observed by the owner. Exactly one consumer.
    nonisolated let events: AsyncStream<RelayEvent>

    init(relayURL: URL, hostID: String, deviceID: String) throws {
        guard var components = URLComponents(url: relayURL, resolvingAgainstBaseURL: false),
              let host = components.host, !host.isEmpty else {
            throw RelayError.invalidEndpoint
        }
        components.scheme = "wss"
        components.path = "/device"
        components.user = nil
        components.password = nil
        components.queryItems = [
            URLQueryItem(name: "host", value: hostID),
            URLQueryItem(name: "device", value: deviceID)
        ]
        guard let endpoint = components.url else {
            throw RelayError.invalidEndpoint
        }
        self.endpoint = endpoint
        self.hostID = hostID
        self.deviceID = deviceID

        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 20
        configuration.timeoutIntervalForResource = 60 * 60
        configuration.waitsForConnectivity = false
        self.session = URLSession(configuration: configuration)

        var continuation: AsyncStream<RelayEvent>.Continuation!
        self.events = AsyncStream(bufferingPolicy: .bufferingNewest(32)) { continuation = $0 }
        self.eventsContinuation = continuation
    }

    /// Opens the socket, announces the device and opens one relay connection.
    @discardableResult
    func open() async throws -> String {
        guard !isClosed else { throw RelayError.notConnected }
        let task = session.webSocketTask(with: endpoint)
        self.task = task
        task.resume()

        let conn = UUID().uuidString.lowercased()
        connectionID = conn
        try await sendFrame(["type": "hello", "host_id": hostID, "device_id": deviceID])
        try await sendFrame(["type": "open", "conn": conn])

        receiveTask = Task { [weak self] in
            await self?.receiveLoop()
        }
        pingTask = Task { [weak self] in
            await self?.pingLoop()
        }
        return conn
    }

    /// Sends one opaque payload inside a relay data frame.
    func send(payload: Data) async throws {
        guard let connectionID, !isClosed else { throw RelayError.notConnected }
        try await sendFrame([
            "type": "data",
            "conn": connectionID,
            "payload": payload.base64EncodedString()
        ])
    }

    func close() async {
        guard !isClosed else { return }
        isClosed = true
        pingTask?.cancel()
        receiveTask?.cancel()
        if let connectionID, let task {
            let frame = #"{"type":"close","conn":"\#(connectionID)"}"#
            _ = try? await task.send(.string(frame))
        }
        task?.cancel(with: .goingAway, reason: nil)
        session.invalidateAndCancel()
        eventsContinuation.finish()
    }

    // MARK: - Private

    private func sendFrame(_ object: [String: String]) async throws {
        guard let task else { throw RelayError.notConnected }
        let data = try JSONSerialization.data(withJSONObject: object)
        guard let text = String(data: data, encoding: .utf8) else {
            throw RelayError.transport("could not encode relay frame")
        }
        do {
            try await task.send(.string(text))
        } catch {
            throw RelayError.transport("send failed")
        }
    }

    private func receiveLoop() async {
        guard let task else { return }
        while !Task.isCancelled && !isClosed {
            do {
                let message = try await task.receive()
                switch message {
                case .string(let text):
                    try handle(text: text)
                case .data(let data):
                    if let text = String(data: data, encoding: .utf8) {
                        try handle(text: text)
                    }
                @unknown default:
                    break
                }
            } catch {
                if !isClosed {
                    eventsContinuation.yield(.failed("receive failed"))
                }
                eventsContinuation.finish()
                return
            }
        }
    }

    private func handle(text: String) throws {
        guard let data = text.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data),
              let frame = object as? [String: Any],
              let type = frame["type"] as? String else {
            throw RelayError.protocolViolation("not a JSON frame")
        }

        switch type {
        case "data":
            guard let payload = frame["payload"] as? String,
                  let bytes = Data(base64Encoded: payload) ?? Base64URL.decode(payload) else {
                throw RelayError.protocolViolation("data frame without base64 payload")
            }
            eventsContinuation.yield(.data(bytes))
        case "close":
            eventsContinuation.yield(.closed(frame["reason"] as? String ?? "closed"))
        case "error":
            let message = frame["message"] as? String
            eventsContinuation.yield(.failed(message ?? "relay error"))
        case "hello", "open":
            eventsContinuation.yield(.opened)
        default:
            // Additive frames are ignored rather than treated as fatal.
            break
        }
    }

    private func pingLoop() async {
        while !Task.isCancelled && !isClosed {
            do {
                try await Task.sleep(nanoseconds: 20 * 1_000_000_000)
            } catch {
                return
            }
            guard let task, !isClosed else { return }
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                task.sendPing { error in
                    if error != nil {
                        self.eventsContinuation.yield(.failed("ping failed"))
                    }
                    continuation.resume()
                }
            }
            if Task.isCancelled { return }
        }
    }
}
