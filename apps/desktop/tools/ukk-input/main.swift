// ukk-input is the tiny macOS input helper used by Remote Screen.
//
// The Desktop process writes one JSON object per line to stdin and never uses
// a shell. The helper reports Accessibility permission in a ready line and
// then posts CoreGraphics events. It is built by CI and shipped at
// resources/ukk-input next to the Desktop executable.
import ApplicationServices
import CoreGraphics
import Foundation

struct Command: Decodable {
    let type: String
    let kind: String?
    let x: Int?
    let y: Int?
    let button: String?
    let code: String?
    let down: Bool?
    let dx: Int?
    let dy: Int?
}

struct Ready: Encodable {
    let type = "ready"
    let accessibility: Bool
}

struct Failure: Encodable {
    let type = "error"
    let message: String
}

let maxCommandBytes = 4096

func writeJSON<T: Encodable>(_ value: T) {
    let encoder = JSONEncoder()
    guard let data = try? encoder.encode(value) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
}

func fail(_ message: String) {
    writeJSON(Failure(message: message))
}

let keyCodes: [String: CGKeyCode] = [
    "KeyA": 0x00, "KeyS": 0x01, "KeyD": 0x02, "KeyF": 0x03,
    "KeyH": 0x04, "KeyG": 0x05, "KeyZ": 0x06, "KeyX": 0x07,
    "KeyC": 0x08, "KeyV": 0x09, "KeyB": 0x0B, "KeyQ": 0x0C,
    "KeyW": 0x0D, "KeyE": 0x0E, "KeyR": 0x0F, "KeyY": 0x10,
    "KeyT": 0x11, "KeyO": 0x1F, "KeyU": 0x20, "KeyI": 0x22,
    "KeyP": 0x23, "KeyL": 0x25, "KeyJ": 0x26, "KeyK": 0x28,
    "KeyN": 0x2D, "KeyM": 0x2E,
    "Digit1": 0x12, "Digit2": 0x13, "Digit3": 0x14, "Digit4": 0x15,
    "Digit6": 0x16, "Digit5": 0x17, "Digit9": 0x19, "Digit7": 0x1A,
    "Digit8": 0x1C, "Digit0": 0x1D,
    "Equal": 0x18, "Minus": 0x1B, "BracketRight": 0x1E, "BracketLeft": 0x21,
    "Quote": 0x27, "Semicolon": 0x29, "Backslash": 0x2A, "Comma": 0x2B,
    "Slash": 0x2C, "Period": 0x2F, "Backquote": 0x32,
    "Enter": 0x24, "Tab": 0x30, "Space": 0x31, "Backspace": 0x33,
    "Escape": 0x35, "MetaRight": 0x36, "MetaLeft": 0x37,
    "ShiftLeft": 0x38, "CapsLock": 0x39, "AltLeft": 0x3A,
    "ControlLeft": 0x3B, "ShiftRight": 0x3C, "AltRight": 0x3D,
    "ControlRight": 0x3E, "Insert": 0x72, "Delete": 0x75,
    "Home": 0x73, "End": 0x77, "PageUp": 0x74, "PageDown": 0x79,
    "ArrowLeft": 0x7B, "ArrowRight": 0x7C, "ArrowDown": 0x7D, "ArrowUp": 0x7E,
    "NumpadDecimal": 0x41, "NumpadMultiply": 0x43, "NumpadAdd": 0x45,
    "NumpadDivide": 0x4B, "NumpadEnter": 0x4C, "NumpadSubtract": 0x4E,
    "Numpad0": 0x52, "Numpad1": 0x53, "Numpad2": 0x54, "Numpad3": 0x55,
    "Numpad4": 0x56, "Numpad5": 0x57, "Numpad6": 0x58, "Numpad7": 0x59,
    "Numpad8": 0x5B, "Numpad9": 0x5C,
    "F1": 0x7A, "F2": 0x78, "F3": 0x63, "F4": 0x76,
    "F5": 0x60, "F6": 0x61, "F7": 0x62, "F8": 0x64,
    "F9": 0x65, "F10": 0x6D, "F11": 0x67, "F12": 0x6F,
]

let mouseButtons: [String: CGMouseButton] = [
    "left": .left,
    "right": .right,
    "middle": .center,
]

let accessibility = AXIsProcessTrusted()
writeJSON(Ready(accessibility: accessibility))

func postPointer(_ command: Command) {
    guard let kind = command.kind else {
        fail("pointer command has no kind")
        return
    }
    let point = CGPoint(x: CGFloat(command.x ?? 0), y: CGFloat(command.y ?? 0))
    let source = CGEventSource(stateID: .hidSystemState)
    var event: CGEvent?

    switch kind {
    case "move":
        event = CGEvent(mouseEventSource: source, mouseType: .mouseMoved,
                        mouseCursorPosition: point, mouseButton: .left)
    case "down", "up":
        let name = command.button ?? "left"
        guard let button = mouseButtons[name] else {
            fail("unsupported mouse button \(name)")
            return
        }
        let type: CGEventType
        switch (name, kind) {
        case ("left", "down"): type = .leftMouseDown
        case ("left", "up"): type = .leftMouseUp
        case ("right", "down"): type = .rightMouseDown
        case ("right", "up"): type = .rightMouseUp
        case (_, "down"): type = .otherMouseDown
        default: type = .otherMouseUp
        }
        event = CGEvent(mouseEventSource: source, mouseType: type,
                        mouseCursorPosition: point, mouseButton: button)
    default:
        fail("unknown pointer kind \(kind)")
        return
    }

    event?.post(tap: .cghidEventTap)
}

func postKey(_ command: Command) {
    guard let code = command.code else {
        fail("key command has no code")
        return
    }
    guard let virtualKey = keyCodes[code] else {
        fail("unsupported key \(code)")
        return
    }
    let down = command.down ?? false
    guard let event = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState),
                              virtualKey: virtualKey, keyDown: down) else {
        fail("could not create key event")
        return
    }
    event.post(tap: .cghidEventTap)
}

func postWheel(_ command: Command) {
    let dx = Int32(clamping: command.dx ?? 0)
    let dy = Int32(clamping: command.dy ?? 0)
    guard let event = CGEvent(scrollWheelEvent2Source: CGEventSource(stateID: .hidSystemState),
                              units: .line, wheelCount: 2,
                              wheel1: dy, wheel2: dx, wheel3: 0) else {
        fail("could not create wheel event")
        return
    }
    event.post(tap: .cghidEventTap)
}

while let line = readLine(strippingNewline: true) {
    if line.utf8.count > maxCommandBytes {
        fail("command too long")
        continue
    }
    guard let data = line.data(using: .utf8),
          let command = try? JSONDecoder().decode(Command.self, from: data) else {
        fail("malformed JSON command")
        continue
    }
    if !accessibility {
        fail("Accessibility permission is not granted")
        continue
    }
    switch command.type {
    case "pointer": postPointer(command)
    case "key": postKey(command)
    case "wheel": postWheel(command)
    default: fail("unknown command type \(command.type)")
    }
}
