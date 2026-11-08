// swift-tools-version:5.9
import PackageDescription

// Local package around the LiveKit WebRTC binary. The 150.7871.05 xcframework is
// downloaded by scripts/fetch-webrtc.sh (checksum-pinned) and is not committed.
let package = Package(
    name: "LiveKitWebRTC",
    platforms: [.iOS(.v13)],
    products: [
        .library(name: "LiveKitWebRTC", targets: ["LiveKitWebRTC"]),
    ],
    targets: [
        .binaryTarget(name: "LiveKitWebRTC", path: "LiveKitWebRTC.xcframework"),
    ]
)
