import CoreGraphics
import Foundation
import ScreenCaptureKit

/// Display metadata only. No pixels are read for `list-displays`.
struct DisplayInfo: Codable, Equatable {
    let id: UInt32
    let width: Int
    let height: Int
    let x: Double
    let y: Double
}

/// Window metadata only. No pixels are read for `list-windows`.
struct WindowInfo: Codable, Equatable {
    let id: UInt32
    let title: String?
    let application: String?
    let processID: Int32?
    let onScreen: Bool
    let layer: Int
    let x: Double
    let y: Double
    let width: Double
    let height: Double
}

enum CaptureService {
    static func listDisplays() async throws -> [DisplayInfo] {
        let content = try await shareableContent()
        return content.displays
            .map {
                DisplayInfo(
                    id: $0.displayID,
                    width: $0.width,
                    height: $0.height,
                    x: Double($0.frame.origin.x),
                    y: Double($0.frame.origin.y)
                )
            }
            .sorted { $0.id < $1.id }
    }

    static func listWindows() async throws -> [WindowInfo] {
        let content = try await shareableContent()
        return content.windows
            .map {
                WindowInfo(
                    id: $0.windowID,
                    title: $0.title,
                    application: $0.owningApplication?.applicationName,
                    processID: $0.owningApplication?.processID,
                    onScreen: $0.isOnScreen,
                    layer: $0.windowLayer,
                    x: Double($0.frame.origin.x),
                    y: Double($0.frame.origin.y),
                    width: Double($0.frame.size.width),
                    height: Double($0.frame.size.height)
                )
            }
            .sorted { $0.id < $1.id }
    }

    /// Capture exactly one frame from one display and write one PNG.
    ///
    /// `SCScreenshotManager.captureImage` is a single synchronous-style capture
    /// request: there is no stream, no frame callback and no retained frame
    /// after `writePNG` returns. Audio is explicitly disabled.
    static func captureOnce(displayID: UInt32, to outputURL: URL) async throws {
        guard #available(macOS 14.0, *) else {
            throw HelperError.unsupportedOS(
                "capture-once requires macOS 14 or newer because it uses SCScreenshotManager"
            )
        }

        let content = try await shareableContent()
        guard let display = content.displays.first(where: { $0.displayID == displayID }) else {
            throw HelperError.captureFailed(
                "display \(displayID) was not found; run list-displays for current ids"
            )
        }

        let filter = SCContentFilter(display: display, excludingWindows: [])
        let configuration = SCStreamConfiguration()
        let pixelWidth = Int(CGDisplayPixelsWide(display.displayID))
        let pixelHeight = Int(CGDisplayPixelsHigh(display.displayID))
        configuration.width = pixelWidth > 0 ? pixelWidth : display.width
        configuration.height = pixelHeight > 0 ? pixelHeight : display.height
        configuration.showsCursor = false
        configuration.capturesAudio = false

        let image: CGImage
        do {
            image = try await SCScreenshotManager.captureImage(
                contentFilter: filter,
                configuration: configuration
            )
        } catch {
            throw HelperError.mapCaptureError(error)
        }

        try OutputPath.writePNG(image, to: outputURL)
    }

    private static func shareableContent() async throws -> SCShareableContent {
        do {
            return try await SCShareableContent.excludingDesktopWindows(
                false,
                onScreenWindowsOnly: false
            )
        } catch {
            throw HelperError.mapCaptureError(error)
        }
    }
}
