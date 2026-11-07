import Foundation

/// A parsed QR pairing invitation. Values are **untrusted input**: this type only
/// validates shape and expiry locally. It never proves Host identity, never
/// establishes trust, and its secret must never be logged or persisted here.
///
/// The documented proposal (docs/PAIRING.md) is illustrative; S2 owns the final
/// cryptographic construction and Host-side single-use enforcement.
struct PairingInvite: Equatable, Sendable, CustomStringConvertible {
    let version: Int
    let relayOrigin: URL
    let hostID: UUID
    let hostPublicKey: String
    let inviteID: String
    let inviteSecret: String
    let expiresAt: Date

    /// Deliberately omits `inviteSecret`.
    var description: String {
        "PairingInvite(v\(version), relay: \(relayOrigin.absoluteString), host: \(hostID.uuidString), invite: \(inviteID))"
    }
}

enum PairingInviteParseError: Error, Equatable, Sendable {
    case malformedJSON
    case unsupportedVersion(Int)
    case missingField(String)
    case invalidRelayOrigin
    case invalidHostID
    case invalidPublicKey
    case invalidInviteIdentifier
    case invalidInviteSecret
    case invalidExpiry
    case expired

    var message: String {
        switch self {
        case .malformedJSON:
            return "Not a valid JSON pairing invite."
        case .unsupportedVersion(let version):
            return "Unsupported invite version \(version). This build understands version 1."
        case .missingField(let field):
            return "The invite is missing the “\(field)” field."
        case .invalidRelayOrigin:
            return "The relay origin must be an https or wss URL with a host."
        case .invalidHostID:
            return "The host identifier is not a UUID."
        case .invalidPublicKey:
            return "The host public key is not a base64url string."
        case .invalidInviteIdentifier:
            return "The invite identifier is empty or malformed."
        case .invalidInviteSecret:
            return "The invite secret is shorter than the minimum length."
        case .invalidExpiry:
            return "The expiry timestamp is not ISO-8601."
        case .expired:
            return "This invitation has expired. Ask the Desktop to generate a new one."
        }
    }
}

enum PairingInviteParser {
    static func parse(payload: String, now: Date) -> Result<PairingInvite, PairingInviteParseError> {
        guard let data = payload.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data),
              let object = root as? [String: Any] else {
            return .failure(.malformedJSON)
        }

        guard let version = object["v"] as? Int else {
            return .failure(.missingField("v"))
        }
        guard version == 1 else {
            return .failure(.unsupportedVersion(version))
        }

        func string(_ key: String) -> Result<String, PairingInviteParseError> {
            guard let value = object[key] as? String, !value.isEmpty else {
                return .failure(.missingField(key))
            }
            return .success(value)
        }

        let relayString: String
        switch string("relay") {
        case .success(let value): relayString = value
        case .failure(let error): return .failure(error)
        }

        guard let relayURL = URL(string: relayString),
              let scheme = relayURL.scheme?.lowercased(),
              scheme == "https" || scheme == "wss",
              let host = relayURL.host,
              !host.isEmpty else {
            return .failure(.invalidRelayOrigin)
        }

        let hostIDString: String
        switch string("host_id") {
        case .success(let value): hostIDString = value
        case .failure(let error): return .failure(error)
        }
        guard let hostID = UUID(uuidString: hostIDString) else {
            return .failure(.invalidHostID)
        }

        let publicKey: String
        switch string("host_public_key") {
        case .success(let value): publicKey = value
        case .failure(let error): return .failure(error)
        }
        let allowedKeyCharacters = CharacterSet(
            charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
        )
        guard publicKey.count >= 32,
              publicKey.rangeOfCharacter(from: allowedKeyCharacters.inverted) == nil else {
            return .failure(.invalidPublicKey)
        }

        let inviteID: String
        switch string("invite_id") {
        case .success(let value): inviteID = value
        case .failure(let error): return .failure(error)
        }
        guard inviteID.count >= 8 else {
            return .failure(.invalidInviteIdentifier)
        }

        let inviteSecret: String
        switch string("invite_secret") {
        case .success(let value): inviteSecret = value
        case .failure(let error): return .failure(error)
        }
        guard inviteSecret.count >= 16 else {
            return .failure(.invalidInviteSecret)
        }

        let expiryString: String
        switch string("expires_at") {
        case .success(let value): expiryString = value
        case .failure(let error): return .failure(error)
        }
        guard let expiresAt = iso8601Date(expiryString) else {
            return .failure(.invalidExpiry)
        }
        guard expiresAt > now else {
            return .failure(.expired)
        }

        return .success(
            PairingInvite(
                version: version,
                relayOrigin: relayURL,
                hostID: hostID,
                hostPublicKey: publicKey,
                inviteID: inviteID,
                inviteSecret: inviteSecret,
                expiresAt: expiresAt
            )
        )
    }

    private static func iso8601Date(_ value: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: value) { return date }

        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        return plain.date(from: value)
    }
}
