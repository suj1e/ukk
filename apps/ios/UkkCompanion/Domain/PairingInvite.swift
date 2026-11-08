import Foundation

/// A v1 pairing invitation. Every value is untrusted input: parsing proves only
/// that the shape is valid and that the invitation has not expired. Trust is
/// established only by the handshake and the six-digit confirmation.
///
/// The invite secret is never logged, displayed or persisted. It exists for the
/// lifetime of one pairing attempt and is zeroized when that attempt ends.
struct PairingInvite: Equatable, Sendable, CustomStringConvertible {
    static let maximumLifetime: TimeInterval = 5 * 60

    let version: Int
    let hostID: String
    let hostEd: Data
    let hostX: Data
    let inviteID: String
    let secret: Data
    let expireMS: Int64
    let relay: URL

    var expiresAt: Date {
        Date(timeIntervalSince1970: Double(expireMS) / 1000)
    }

    /// Deliberately omits `secret`.
    var description: String {
        "PairingInvite(v\(version), relay: \(relay.absoluteString), host: \(hostID), invite: \(inviteID))"
    }
}

enum PairingInviteParseError: Error, Equatable, Sendable {
    case malformedJSON
    case unknownField(String)
    case unsupportedVersion(Int)
    case missingField(String)
    case invalidRelayOrigin
    case invalidHostID
    case invalidKey(String)
    case invalidInviteIdentifier
    case invalidInviteSecret
    case invalidExpiry
    case expired
    case lifetimeTooLong

    var message: String {
        switch self {
        case .malformedJSON:
            return "Not a valid pairing invite. Paste the exact JSON from the Desktop."
        case .unknownField(let field):
            return "The invite contains an unsupported field “\(field)”."
        case .unsupportedVersion(let version):
            return "Unsupported invite version \(version). This build understands version 1."
        case .missingField(let field):
            return "The invite is missing the “\(field)” field."
        case .invalidRelayOrigin:
            return "The relay must be an absolute wss:// URL without credentials."
        case .invalidHostID:
            return "The host identifier is malformed."
        case .invalidKey(let field):
            return "The host “\(field)” key must be 32 base64url bytes."
        case .invalidInviteIdentifier:
            return "The invite identifier is empty or malformed."
        case .invalidInviteSecret:
            return "The invite secret must be 32 base64url bytes."
        case .invalidExpiry:
            return "The invite expiry is not a Unix millisecond timestamp."
        case .expired:
            return "This invitation has expired. Ask the Desktop to generate a new one."
        case .lifetimeTooLong:
            return "This invitation claims a lifetime longer than five minutes and is refused."
        }
    }
}

enum PairingInviteParser {
    private static let allowedFields: Set<String> = [
        "v", "host_id", "host_ed", "host_x", "invite_id", "secret", "expire_ms", "relay"
    ]

    /// Parses an invite. `now` is injectable so expiry is deterministic in tests.
    static func parse(payload: String, now: Date = Date()) -> Result<PairingInvite, PairingInviteParseError> {
        guard let data = payload.data(using: .utf8) else {
            return .failure(.malformedJSON)
        }
        let object: [String: Any]
        do {
            object = try StrictJSON.object(data, allowed: allowedFields)
        } catch StrictJSON.Failure.malformed {
            return .failure(.malformedJSON)
        } catch StrictJSON.Failure.unknownField(let field) {
            return .failure(.unknownField(field))
        } catch {
            return .failure(.malformedJSON)
        }

        guard let versionNumber = object["v"] as? NSNumber, !(object["v"] is Bool) else {
            return .failure(.missingField("v"))
        }
        let version = versionNumber.intValue
        guard version == 1 else {
            return .failure(.unsupportedVersion(version))
        }

        func string(_ key: String) -> Result<String, PairingInviteParseError> {
            guard let value = object[key] as? String, !value.isEmpty else {
                return .failure(.missingField(key))
            }
            return .success(value)
        }

        func key(_ field: String) -> Result<Data, PairingInviteParseError> {
            switch string(field) {
            case .success(let text):
                guard let raw = Base64URL.decode(text), raw.count == 32 else {
                    return .failure(.invalidKey(field))
                }
                return .success(raw)
            case .failure(let error):
                return .failure(error)
            }
        }

        let relayString: String
        switch string("relay") {
        case .success(let value): relayString = value
        case .failure(let error): return .failure(error)
        }

        guard let relayURL = URL(string: relayString),
              let scheme = relayURL.scheme?.lowercased(),
              scheme == "wss",
              let host = relayURL.host,
              !host.isEmpty,
              relayURL.user == nil,
              relayURL.password == nil else {
            return .failure(.invalidRelayOrigin)
        }

        let hostID: String
        switch string("host_id") {
        case .success(let value): hostID = value
        case .failure(let error): return .failure(error)
        }
        guard !hostID.isEmpty, hostID.count <= 128 else {
            return .failure(.invalidHostID)
        }

        let hostEd: Data
        switch key("host_ed") {
        case .success(let value): hostEd = value
        case .failure(let error): return .failure(error)
        }

        let hostX: Data
        switch key("host_x") {
        case .success(let value): hostX = value
        case .failure(let error): return .failure(error)
        }

        let inviteID: String
        switch string("invite_id") {
        case .success(let value): inviteID = value
        case .failure(let error): return .failure(error)
        }
        guard inviteID.count >= 8, inviteID.count <= 128 else {
            return .failure(.invalidInviteIdentifier)
        }

        let secret: Data
        switch string("secret") {
        case .success(let text):
            guard let decoded = Base64URL.decode(text), decoded.count == 32 else {
                return .failure(.invalidInviteSecret)
            }
            secret = decoded
        case .failure(let error):
            return .failure(error)
        }

        guard let expiryNumber = object["expire_ms"] as? NSNumber, !(object["expire_ms"] is Bool) else {
            return .failure(.missingField("expire_ms"))
        }
        let expireMS = expiryNumber.int64Value
        let nowMS = Int64((now.timeIntervalSince1970 * 1000).rounded())
        guard expireMS > 0 else {
            return .failure(.invalidExpiry)
        }
        guard expireMS > nowMS else {
            return .failure(.expired)
        }
        guard expireMS - nowMS <= Int64(PairingInvite.maximumLifetime * 1000) else {
            return .failure(.lifetimeTooLong)
        }

        return .success(
            PairingInvite(
                version: version,
                hostID: hostID,
                hostEd: hostEd,
                hostX: hostX,
                inviteID: inviteID,
                secret: secret,
                expireMS: expireMS,
                relay: relayURL
            )
        )
    }
}

/// Strict JSON object decoding shared by the invite and handshake messages.
/// Rejects non-objects, trailing data and unknown fields without ever echoing
/// input values in errors.
enum StrictJSON {
    enum Failure: Error, Equatable {
        case malformed
        case unknownField(String)
    }

    static func object(_ data: Data, allowed: Set<String>? = nil) throws -> [String: Any] {
        let decoded: Any
        do {
            decoded = try JSONSerialization.jsonObject(with: data)
        } catch {
            throw Failure.malformed
        }
        guard let object = decoded as? [String: Any] else {
            throw Failure.malformed
        }
        if let allowed {
            for key in object.keys where !allowed.contains(key) {
                throw Failure.unknownField(key)
            }
        }
        return object
    }

    static func base64(_ value: Any?, field: String) throws -> Data {
        guard let text = value as? String, let data = Base64URL.decode(text) else {
            throw Failure.malformed
        }
        _ = field
        return data
    }

    static func key32(_ value: Any?) throws -> Data {
        let data = try base64(value, field: "")
        guard data.count == 32 else { throw Failure.malformed }
        return data
    }
}
