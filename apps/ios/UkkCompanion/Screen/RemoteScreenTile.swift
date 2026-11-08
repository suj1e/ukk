import LiveKitWebRTC
import SwiftUI

/// Remote Screen tile. Media comes from a real RTCPeerConnection; when the Host
/// does not send a track the tile says so instead of showing a poster.
struct RemoteScreenTile: View {
    let client: RemoteHostClient?
    let isConnectionLive: Bool
    let canRequestControl: Bool

    @StateObject private var controller: RemoteScreenController
    @State private var isExpanded = false
    @State private var isDragging = false

    init(
        client: RemoteHostClient?,
        sessionID: String,
        isConnectionLive: Bool,
        canRequestControl: Bool
    ) {
        self.client = client
        self.isConnectionLive = isConnectionLive
        self.canRequestControl = canRequestControl
        _controller = StateObject(
            wrappedValue: RemoteScreenController(
                client: client,
                sessionID: sessionID,
                shouldRequestControl: canRequestControl
            )
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            if let track = controller.videoTrack {
                video(track: track)
            }
            if !isConnectionLive {
                Text("Remote Screen needs a live Host connection.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("remoteScreenOffline")
            } else if controller.state.isFailure {
                Text(controller.state.label)
                    .font(.caption2)
                    .foregroundStyle(Color.orange)
                    .accessibilityIdentifier("remoteScreenUnavailable")
            }
        }
        .padding(10)
        .background(
            Color(.secondarySystemGroupedBackground),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color(.separator), lineWidth: 0.5)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("remoteScreenTile")
        .onDisappear {
            controller.stop()
        }
        .onChange(of: client.map(ObjectIdentifier.init)) { _, _ in
            controller.updateClient(client)
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "rectangle.on.rectangle.angled")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("Remote Screen")
                    .font(.caption.weight(.semibold))
                Text(controller.state.label)
                    .font(.caption2)
                    .foregroundStyle(controller.state.isFailure ? Color.orange : Color.secondary)
                    .lineLimit(1)
                    .accessibilityIdentifier("remoteScreenState")
            }
            Spacer(minLength: 0)

            if !isConnectionLive || controller.videoTrack == nil {
                if !controller.state.isActive || controller.state.isFailure {
                    Button("Start") { controller.start() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(!isConnectionLive)
                        .accessibilityIdentifier("remoteScreenStart")
                }
            }

            if controller.videoTrack != nil {
                Button {
                    isExpanded.toggle()
                } label: {
                    Image(systemName: isExpanded ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("remoteScreenExpand")
            }

            if controller.state.isActive {
                Button(role: .destructive) {
                    isExpanded = false
                    controller.stop()
                } label: {
                    Image(systemName: "stop.circle.fill")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("remoteScreenStop")
                .accessibilityLabel("Stop Remote Screen")
            }
        }
    }

    private func video(track: LKRTCVideoTrack) -> some View {
        ZStack {
            RemoteVideoView(track: track)
                .frame(height: isExpanded ? 260 : 120)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))

            if controller.controlGranted {
                GeometryReader { geometry in
                    Color.clear
                        .contentShape(Rectangle())
                        .gesture(
                            DragGesture(minimumDistance: 0)
                                .onChanged { value in
                                    if !isDragging {
                                        isDragging = true
                                        controller.pointerDown(at: value.location, in: geometry.size)
                                    } else {
                                        controller.pointerMove(at: value.location, in: geometry.size)
                                    }
                                }
                                .onEnded { value in
                                    controller.pointerUp(at: value.location, in: geometry.size)
                                    isDragging = false
                                }
                        )
                        .accessibilityIdentifier("remoteScreenControlSurface")
                }
            }

            if !controller.controlGranted {
                VStack {
                    Spacer()
                    HStack {
                        Text("View only")
                            .font(.caption2.weight(.semibold))
                            .padding(.horizontal, 6)
                            .padding(.vertical, 3)
                            .background(.ultraThinMaterial, in: Capsule())
                        Spacer()
                    }
                }
                .padding(6)
                .allowsHitTesting(false)
            }
        }
    }
}
