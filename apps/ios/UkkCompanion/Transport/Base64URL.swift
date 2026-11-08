import Foundation

/// Unpadded base64url (RFC 4648 §5), the only encoding the remote protocol uses
/// for byte strings. Decoding also accepts the padded form produced by some
/// platform libraries.
enum Base64URL {
    static func encode(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func decode(_ text: String) -> Data? {
        guard !text.isEmpty else { return nil }
        var value = text
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = value.count % 4
        if remainder > 0 {
            value += String(repeating: "=", count: 4 - remainder)
        }
        guard let data = Data(base64Encoded: value) else { return nil }
        // Canonical input must not contain characters outside the URL alphabet.
        let allowed = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_=")
        guard text.rangeOfCharacter(from: allowed.inverted) == nil else { return nil }
        return data
    }
}
