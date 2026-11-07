// swift-tools-version: 5.9
//
// Bounded ScreenCaptureKit capture helper for the Remote Screen M1 spike.
//
// Build:  swift build --package-path plugins/remote-screen/macos
// Test:   swift test  --package-path plugins/remote-screen/macos
//
// The helper links ScreenCaptureKit directly. Real capture is not exercised in
// CI because an unsigned SwiftPM executable has no Screen Recording grant; the
// capture test skips with an explicit NOT VERIFIED message unless the operator
// opts in with UKK_RUN_SCREEN_CAPTURE_TESTS=1 on a manually granted machine.
import PackageDescription

let package = Package(
    name: "RemoteScreenHelper",
    platforms: [
        // SCScreenshotManager (used for one-frame capture) is macOS 14+.
        // The package builds against 13 so older macOS reaches the helper's
        // explicit unsupported-OS exit instead of failing to launch at load
        // time.
        .macOS(.v13)
    ],
    products: [
        .executable(name: "ukk-remote-screen-helper", targets: ["RemoteScreenHelper"])
    ],
    targets: [
        .executableTarget(
            name: "RemoteScreenHelper",
            path: "Sources/RemoteScreenHelper"
        ),
        .testTarget(
            name: "RemoteScreenHelperTests",
            dependencies: ["RemoteScreenHelper"],
            path: "Tests/RemoteScreenHelperTests"
        )
    ]
)
