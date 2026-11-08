import Foundation
import LiveKitWebRTC
import SwiftUI
import UIKit

/// WebRTC answerer for the Remote Screen plugin. The Host sends an SDP offer and
/// ICE configuration over the encrypted RPC channel; this controller answers,
/// renders the remote video track and sends pointer input only when the Host
/// reports a control grant.
@MainActor
final class RemoteScreenController: NSObject, ObservableObject {
    enum State: Equatable {
        case idle
        case requesting
        case negotiating
        case viewing(control: Bool)
        case unavailable(String)
        case failed(String)
        case stopped

        var label: String {
            switch self {
            case .idle: return "Not started"
            case .requesting: return "Requesting screen…"
            case .negotiating: return "Negotiating video…"
            case .viewing(let control): return control ? "Viewing · control granted" : "Viewing · view only"
            case .unavailable(let reason): return reason
            case .failed(let reason): return reason
            case .stopped: return "Stopped"
            }
        }

        var isActive: Bool {
            switch self {
            case .requesting, .negotiating, .viewing: return true
            default: return false
            }
        }

        var isFailure: Bool {
            switch self {
            case .failed, .unavailable: return true
            default: return false
            }
        }
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var videoTrack: LKRTCVideoTrack?
    @Published private(set) var remoteSize: CGSize = .zero
    @Published private(set) var controlGranted = false

    private var client: RemoteHostClient?
    private let sessionID: String
    private let shouldRequestControl: Bool

    private var factory: LKRTCPeerConnectionFactory?
    private var peerConnection: LKRTCPeerConnection?
    private var probe: VideoFrameProbe?
    private var startTask: Task<Void, Never>?
    private var videoTimeoutTask: Task<Void, Never>?
    private var inputChain: Task<Void, Never>?
    private var hasAttachedVideo = false

    init(client: RemoteHostClient?, sessionID: String, shouldRequestControl: Bool) {
        self.client = client
        self.sessionID = sessionID
        self.shouldRequestControl = shouldRequestControl
        super.init()
    }

    /// Replaces the RPC client after a reconnect and stops stale media.
    func updateClient(_ newClient: RemoteHostClient?) {
        guard client !== newClient else { return }
        client = newClient
        if state.isActive {
            stop()
        }
    }

    func start() {
        guard !state.isActive else { return }
        guard let client else {
            state = .unavailable("No Host connection.")
            return
        }
        state = .requesting
        hasAttachedVideo = false

        startTask?.cancel()
        startTask = Task { [weak self] in
            guard let self else { return }
            do {
                let session = try await client.screenStart(
                    sessionID: self.sessionID,
                    control: self.shouldRequestControl
                )
                guard !Task.isCancelled else { return }
                self.controlGranted = session.control
                guard session.view else {
                    self.state = .unavailable("The Host did not grant screen viewing.")
                    return
                }
                try await self.negotiate(session, client: client)
            } catch {
                guard !Task.isCancelled else { return }
                self.state = .failed(Self.message(for: error))
                self.teardownPeer()
            }
        }
    }

    /// Tears down media immediately and asks the Host to stop capture.
    func stop() {
        startTask?.cancel()
        startTask = nil
        videoTimeoutTask?.cancel()
        videoTimeoutTask = nil
        inputChain?.cancel()
        inputChain = nil
        teardownPeer()
        controlGranted = false
        state = .stopped
        if let client {
            Task { try? await client.screenStop(sessionID: sessionID) }
        }
    }

    // MARK: - Input

    func pointerDown(at point: CGPoint, in viewSize: CGSize, button: Int = 0) {
        sendPointer(point: point, viewSize: viewSize, down: true, button: button)
    }

    func pointerMove(at point: CGPoint, in viewSize: CGSize) {
        sendPointer(point: point, viewSize: viewSize, down: true, button: 0)
    }

    func pointerUp(at point: CGPoint, in viewSize: CGSize) {
        sendPointer(point: point, viewSize: viewSize, down: false, button: 0)
    }

    private func sendPointer(point: CGPoint, viewSize: CGSize, down: Bool, button: Int) {
        guard controlGranted, let client, viewSize.width > 0, viewSize.height > 0 else { return }
        let scale = remoteSize.width > 0 && remoteSize.height > 0 ? remoteSize : viewSize
        let x = Double(point.x / viewSize.width) * Double(scale.width)
        let y = Double(point.y / viewSize.height) * Double(scale.height)
        enqueue(client: client, events: [.pointer(x: x, y: y, button: button, down: down)])
    }

    func sendWheel(deltaX: Double, deltaY: Double) {
        guard controlGranted, let client else { return }
        enqueue(client: client, events: [.wheel(deltaX: deltaX, deltaY: deltaY)])
    }

    func sendKey(code: String, down: Bool) {
        guard controlGranted, let client else { return }
        enqueue(client: client, events: [.key(code: code, down: down)])
    }

    /// Serialises input RPCs so a down event can never overtake its move/up.
    private func enqueue(client: RemoteHostClient, events: [ScreenInputEvent]) {
        let previous = inputChain
        inputChain = Task {
            _ = await previous?.value
            try? await client.screenInput(sessionID: sessionID, events: events)
        }
    }

    // MARK: - Negotiation

    private func negotiate(_ session: ScreenSessionRecord, client: RemoteHostClient) async throws {
        let factory = LKRTCPeerConnectionFactory()
        self.factory = factory

        let configuration = LKRTCConfiguration()
        configuration.iceServers = session.iceServers.map {
            LKRTCIceServer(
                urlStrings: $0.urls,
                username: $0.username,
                credential: $0.credential
            )
        }
        configuration.sdpSemantics = .unifiedPlan
        configuration.continualGatheringPolicy = .gatherContinually

        let constraints = LKRTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        guard let peer = factory.peerConnection(
            with: configuration,
            constraints: constraints,
            delegate: self
        ) else {
            throw RemoteAPIError.malformedResponse
        }
        peerConnection = peer
        state = .negotiating

        let offer = LKRTCSessionDescription(type: .offer, sdp: session.sdp)
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            peer.setRemoteDescription(offer) { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }

        let answer: LKRTCSessionDescription = try await withCheckedThrowingContinuation { continuation in
            peer.answer(for: constraints) { description, error in
                if let description {
                    continuation.resume(returning: description)
                } else {
                    continuation.resume(throwing: error ?? RemoteAPIError.malformedResponse)
                }
            }
        }

        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            peer.setLocalDescription(answer) { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }

        try await client.screenAnswer(sessionID: sessionID, sdp: answer.sdp)

        videoTimeoutTask?.cancel()
        videoTimeoutTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 12 * 1_000_000_000)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                guard let self, !self.hasAttachedVideo else { return }
                self.state = .unavailable("The Host negotiated a screen session without a video track.")
            }
        }
    }

    private func attach(track: LKRTCVideoTrack) {
        guard !hasAttachedVideo else { return }
        hasAttachedVideo = true
        videoTimeoutTask?.cancel()
        videoTimeoutTask = nil

        let probe = VideoFrameProbe { [weak self] size in
            Task { @MainActor in
                self?.remoteSize = size
            }
        }
        track.add(probe)
        self.probe = probe
        videoTrack = track
        state = .viewing(control: controlGranted)
    }

    private func teardownPeer() {
        if let track = videoTrack, let probe {
            track.remove(probe)
        }
        probe = nil
        videoTrack = nil
        remoteSize = .zero
        peerConnection?.close()
        peerConnection = nil
        factory = nil
        hasAttachedVideo = false
    }

    private static func message(for error: Error) -> String {
        if let error = error as? RemoteAPIError { return error.message }
        if let error = error as? SecureChannelError { return error.message }
        return "Remote Screen failed: \(error.localizedDescription)"
    }
}

// MARK: - LKRTCPeerConnectionDelegate

extension RemoteScreenController: LKRTCPeerConnectionDelegate {
    nonisolated func peerConnection(
        _ peerConnection: LKRTCPeerConnection,
        didAdd receiver: LKRTCRtpReceiver,
        streams: [LKRTCMediaStream]
    ) {
        guard let track = receiver.track as? LKRTCVideoTrack else { return }
        Task { @MainActor [weak self] in
            self?.attach(track: track)
        }
    }

    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didAdd stream: LKRTCMediaStream) {
        guard let track = stream.videoTracks.first else { return }
        Task { @MainActor [weak self] in
            self?.attach(track: track)
        }
    }

    nonisolated func peerConnection(
        _ peerConnection: LKRTCPeerConnection,
        didChange newState: LKRTCIceConnectionState
    ) {
        switch newState {
        case .failed:
            Task { @MainActor [weak self] in
                self?.state = .failed("The media connection failed.")
                self?.teardownPeer()
            }
        default:
            break
        }
    }

    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didChange stateChanged: LKRTCSignalingState) {}
    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didRemove stream: LKRTCMediaStream) {}
    nonisolated func peerConnectionShouldNegotiate(_ peerConnection: LKRTCPeerConnection) {}
    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didChange newState: LKRTCIceGatheringState) {}
    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didGenerate candidate: LKRTCIceCandidate) {}
    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didRemove candidates: [LKRTCIceCandidate]) {}
    nonisolated func peerConnection(_ peerConnection: LKRTCPeerConnection, didOpen dataChannel: LKRTCDataChannel) {}
}

/// Records the decoded frame size so pointer input can be mapped into the
/// captured display's pixel space.
private final class VideoFrameProbe: NSObject, LKRTCVideoRenderer {
    private let onSize: (CGSize) -> Void
    private var lastSize: CGSize = .zero

    init(onSize: @escaping (CGSize) -> Void) {
        self.onSize = onSize
    }

    func setSize(_ size: CGSize) {}

    func renderFrame(_ frame: LKRTCVideoFrame?) {
        guard let frame else { return }
        let size = CGSize(width: CGFloat(frame.width), height: CGFloat(frame.height))
        guard size != lastSize else { return }
        lastSize = size
        onSize(size)
    }
}

/// Wraps LKRTCMTLVideoView so SwiftUI can attach the remote track.
struct RemoteVideoView: UIViewRepresentable {
    let track: LKRTCVideoTrack?

    func makeUIView(context: Context) -> LKRTCMTLVideoView {
        let view = LKRTCMTLVideoView()
        view.videoContentMode = .scaleAspectFit
        view.backgroundColor = .black
        view.clipsToBounds = true
        return view
    }

    func updateUIView(_ uiView: LKRTCMTLVideoView, context: Context) {
        if context.coordinator.track !== track {
            context.coordinator.track?.remove(uiView)
            context.coordinator.track = track
            track?.add(uiView)
        }
    }

    static func dismantleUIView(_ uiView: LKRTCMTLVideoView, coordinator: Coordinator) {
        coordinator.track?.remove(uiView)
        coordinator.track = nil
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator {
        var track: LKRTCVideoTrack?
    }
}
