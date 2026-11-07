// Reports whether a process owns a real CoreGraphics window.
//
// Usage: window_probe <pid>
//
// Exits 0 when at least one layer-0 window with non-zero bounds belongs to
// <pid>, 1 when none does (details are still printed as JSON), and 64 on bad
// usage. No screen-recording permission is needed: window owner, number,
// layer, bounds and on-screen state are public CGWindowList fields.

import CoreGraphics
import Foundation

let usage = "usage: window_probe <pid>\n"

guard CommandLine.arguments.count == 2, let pid = Int32(CommandLine.arguments[1]) else {
    FileHandle.standardError.write(Data(usage.utf8))
    exit(64)
}

func windowList(_ options: CGWindowListOption) -> [[String: Any]] {
    (CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]]) ?? []
}

func describe(_ windows: [[String: Any]]) -> [[String: Any]] {
    windows.compactMap { info in
        let number = (info[kCGWindowNumber as String] as? NSNumber)?.intValue ?? -1
        let layer = (info[kCGWindowLayer as String] as? NSNumber)?.intValue ?? -1
        let alpha = (info[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? -1
        let onscreen = (info[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue ?? false
        let bounds = info[kCGWindowBounds as String] as? [String: Any] ?? [:]
        let width = (bounds["Width"] as? NSNumber)?.doubleValue ?? 0
        let height = (bounds["Height"] as? NSNumber)?.doubleValue ?? 0
        return [
            "number": number,
            "layer": layer,
            "alpha": alpha,
            "onscreen": onscreen,
            "width": width,
            "height": height,
        ]
    }
}

func appWindows(in list: [[String: Any]]) -> [[String: Any]] {
    list.filter { info in
        guard (info[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid else {
            return false
        }
        guard (info[kCGWindowLayer as String] as? NSNumber)?.intValue == 0 else {
            return false
        }
        let bounds = info[kCGWindowBounds as String] as? [String: Any] ?? [:]
        let width = (bounds["Width"] as? NSNumber)?.doubleValue ?? 0
        let height = (bounds["Height"] as? NSNumber)?.doubleValue ?? 0
        return width >= 1 && height >= 1
    }
}

// optionAll also reports windows while the screen is locked or the display is
// asleep; optionOnScreenOnly tells us whether WindowServer is compositing.
let allWindows = windowList([.optionAll, .excludeDesktopElements])
let onscreenWindows = windowList([.optionOnScreenOnly, .excludeDesktopElements])
let owned = describe(appWindows(in: allWindows))
let ownedOnscreen = describe(appWindows(in: onscreenWindows))

let report: [String: Any] = [
    "pid": Int(pid),
    "windowserver_all_windows": allWindows.count,
    "windowserver_onscreen_windows": onscreenWindows.count,
    "app_windows": owned,
    "app_onscreen_windows": ownedOnscreen,
]

let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
FileHandle.standardOutput.write(data)
FileHandle.standardOutput.write(Data("\n".utf8))

exit(owned.isEmpty ? 1 : 0)
