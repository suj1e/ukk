import Foundation

/// APNs is **not** implemented, registered or verified in this build. No push
/// entitlement, device token registration, background fetch or silent-push
/// handler exists. These declarations are the iOS-side interface contract the
/// future notification lane must satisfy.
///
/// Notification metadata leakage mitigation (docs/SECURITY.md): a push payload
/// may carry only an opaque hint identifier. Message text, session titles, file
/// names, paths, pairing codes and secrets must never appear in a payload.
struct OpaquePushHint: Equatable, Sendable {
    let hintIdentifier: String
}

protocol PushWakeHintHandling: AnyObject {
    /// Called after the app has authenticated in the foreground. The hint only
    /// tells the client to refresh; it is never treated as authoritative state.
    func handleOpaqueHint(_ hint: OpaquePushHint)
}

enum PushHintContract {
    /// The only allow-listed APNs key understood by the client.
    static let allowedKey = "ukk_hint_id"

    /// Reads only the opaque hint. Any other key is ignored, including ones that
    /// would leak user content.
    static func parse(userInfo: [AnyHashable: Any]) -> OpaquePushHint? {
        guard let raw = userInfo[allowedKey] as? String else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return OpaquePushHint(hintIdentifier: trimmed)
    }

    /// Returns only the allow-listed field, dropping every other payload entry.
    static func sanitized(userInfo: [AnyHashable: Any]) -> [String: String] {
        guard let hint = parse(userInfo: userInfo) else { return [:] }
        return [allowedKey: hint.hintIdentifier]
    }
}
