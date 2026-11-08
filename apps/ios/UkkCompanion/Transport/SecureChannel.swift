import CryptoKit
import Foundation
import Security

// MARK: - Device identity

/// The long-lived device identity: an Ed25519 signing seed and an X25519 static
/// private key. Seeds are only ever handed to the Keychain; this type is the
/// in-memory working copy.
struct DeviceIdentity: Sendable {
    let edSeed: Data
    let xPrivate: Data

    init(edSeed: Data, xPrivate: Data) throws {
        guard edSeed.count == 32, xPrivate.count == 32 else {
            throw SecureChannelError.invalidIdentity
        }
        guard !edSeed.allSatisfy({ $0 == 0 }), !xPrivate.allSatisfy({ $0 == 0 }) else {
            throw SecureChannelError.invalidIdentity
        }
        _ = try Curve25519.Signing.PrivateKey(rawRepresentation: edSeed)
        _ = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: xPrivate)
        self.edSeed = edSeed
        self.xPrivate = xPrivate
    }

    static func generate() throws -> DeviceIdentity {
        let signing = Curve25519.Signing.PrivateKey()
        let agreement = Curve25519.KeyAgreement.PrivateKey()
        return try DeviceIdentity(
            edSeed: signing.rawRepresentation,
            xPrivate: agreement.rawRepresentation
        )
    }

    var edPrivateKey: Curve25519.Signing.PrivateKey {
        // Validated in init.
        try! Curve25519.Signing.PrivateKey(rawRepresentation: edSeed)
    }

    var xPrivateKey: Curve25519.KeyAgreement.PrivateKey {
        try! Curve25519.KeyAgreement.PrivateKey(rawRepresentation: xPrivate)
    }

    var edPublic: Data { edPrivateKey.publicKey.rawRepresentation }
    var xPublic: Data { xPrivateKey.publicKey.rawRepresentation }
}

/// The pinned public identity of one Host.
struct HostPinnedIdentity: Equatable, Sendable {
    let hostID: String
    let hostEd: Data
    let hostX: Data
    let relayURL: URL

    init(hostID: String, hostEd: Data, hostX: Data, relayURL: URL) throws {
        guard hostEd.count == 32, hostX.count == 32 else {
            throw SecureChannelError.invalidHostIdentity
        }
        self.hostID = hostID
        self.hostEd = hostEd
        self.hostX = hostX
        self.relayURL = relayURL
    }
}

// MARK: - Errors

enum SecureChannelError: Error, Equatable, Sendable {
    case invalidIdentity
    case invalidHostIdentity
    case malformedMessage
    case unexpectedMessage(String)
    case handshakeIdentityMismatch
    case invalidSignature
    case invalidProof
    case invalidEnvelope
    case envelopeVersion(Int)
    case envelopeTooLarge
    case envelopeAuthentication
    case envelopeReplay
    case envelopeGap
    case envelopeStream(UInt32)
    case counterExhausted

    var message: String {
        switch self {
        case .invalidIdentity:
            return "The device identity is malformed."
        case .invalidHostIdentity:
            return "The Host identity is malformed."
        case .malformedMessage:
            return "The peer sent a malformed handshake message."
        case .unexpectedMessage(let type):
            return "The peer sent an unexpected “\(type)” message."
        case .handshakeIdentityMismatch:
            return "The Host identity does not match the invitation. Pairing was refused."
        case .invalidSignature:
            return "A handshake signature failed verification."
        case .invalidProof:
            return "The pairing proof failed verification. The Desktop did not accept this pairing."
        case .invalidEnvelope:
            return "The encrypted envelope is malformed."
        case .envelopeVersion(let version):
            return "Unsupported envelope version \(version)."
        case .envelopeTooLarge:
            return "The encrypted envelope exceeds the 1 MiB plaintext limit."
        case .envelopeAuthentication:
            return "The encrypted envelope failed authentication."
        case .envelopeReplay:
            return "A replayed encrypted envelope was rejected."
        case .envelopeGap:
            return "An encrypted envelope arrived outside the replay window."
        case .envelopeStream(let stream):
            return "An envelope arrived on unexpected stream \(stream)."
        case .counterExhausted:
            return "The channel counter space is exhausted."
        }
    }
}

// MARK: - Handshake cryptography

/// Deterministic transcript and key derivation for the fixed ukk handshake.
/// Everything here is pure so tests can compare fixed keys byte for byte.
enum HandshakeCrypto {
    static let versionLabel = Data("ukk-handshake-v1".utf8)
    static let resumeLabel = Data("ukk-resume-v1".utf8)
    static let saltLabel = Data("ukk-salt-v1".utf8)
    static let keyInfo = Data("ukk-confirm|ukk-c2s|ukk-s2c".utf8)
    static let codeInfo = Data("ukk-code".utf8)
    static let s1ProofLabel = Data("ukk-s1-proof".utf8)
    static let c2ProofLabel = Data("ukk-c2-proof".utf8)
    static let r2ProofLabel = Data("ukk-r2-proof".utf8)
    static let r3ProofLabel = Data("ukk-r3-proof".utf8)

    static func sha256(_ data: Data) -> Data {
        Data(SHA256.hash(data: data))
    }

    static func concat(_ parts: [Data]) -> Data {
        var result = Data()
        for part in parts { result.append(part) }
        return result
    }

    static func bigEndian(_ value: UInt64) -> Data {
        var big = value.bigEndian
        return withUnsafeBytes(of: &big) { Data($0) }
    }

    static func bigEndian(_ value: UInt32) -> Data {
        var big = value.bigEndian
        return withUnsafeBytes(of: &big) { Data($0) }
    }

    /// Prefix a client can sign before the Host ephemeral exists.
    static func pairingClientPrefix(
        invite: PairingInvite,
        clientEd: Data,
        clientX: Data,
        ephClient: Data
    ) -> Data {
        sha256(concat([
            versionLabel,
            Data(invite.hostID.utf8),
            Data(invite.inviteID.utf8),
            invite.hostEd,
            invite.hostX,
            clientEd,
            clientX,
            ephClient,
            bigEndian(UInt64(invite.expireMS))
        ]))
    }

    static func pairingTranscript(
        invite: PairingInvite,
        clientEd: Data,
        clientX: Data,
        ephHost: Data,
        ephClient: Data
    ) -> Data {
        sha256(concat([
            versionLabel,
            Data(invite.hostID.utf8),
            Data(invite.inviteID.utf8),
            invite.hostEd,
            invite.hostX,
            clientEd,
            clientX,
            ephHost,
            ephClient,
            bigEndian(UInt64(invite.expireMS))
        ]))
    }

    static func resumeSignatureMessage(
        hostID: String,
        deviceEd: Data,
        deviceX: Data,
        ephClient: Data,
        nonce: Data
    ) -> Data {
        sha256(concat([
            resumeLabel,
            Data(hostID.utf8),
            deviceEd,
            deviceX,
            ephClient,
            nonce
        ]))
    }

    static func resumeTranscript(
        hostID: String,
        deviceEd: Data,
        deviceX: Data,
        ephHost: Data,
        ephClient: Data,
        nonceClient: Data,
        nonceServer: Data
    ) -> Data {
        sha256(concat([
            resumeLabel,
            Data(hostID.utf8),
            deviceEd,
            deviceX,
            ephHost,
            ephClient,
            nonceClient,
            nonceServer
        ]))
    }

    struct Keys: Sendable {
        let confirm: Data
        let c2s: Data
        let s2c: Data
        let code: String
    }

    static func derive(ikm: Data, transcript: Data) throws -> Keys {
        let salt = sha256(concat([saltLabel, transcript]))
        let keyMaterial = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: ikm),
            salt: salt,
            info: keyInfo,
            outputByteCount: 96
        )
        let raw = keyMaterial.withUnsafeBytes { Data($0) }
        guard raw.count == 96 else { throw SecureChannelError.malformedMessage }

        let codeMaterial = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: ikm),
            salt: salt,
            info: codeInfo,
            outputByteCount: 4
        )
        let codeBytes = Data(codeMaterial.withUnsafeBytes { Data($0) })
        let value = codeBytes.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) } % 1_000_000
        return Keys(
            confirm: raw.prefix(32),
            c2s: raw.dropFirst(32).prefix(32),
            s2c: raw.dropFirst(64).prefix(32),
            code: String(format: "%06d", value)
        )
    }

    static func nonce(transcript: Data, label: Data) -> Data {
        Data(sha256(concat([transcript, label])).prefix(12))
    }

    /// ChaCha20-Poly1305 with empty plaintext, matching the Go AEAD output
    /// (ciphertext || tag, nonce implied by the caller).
    static func proof(key: Data, transcript: Data, label: Data) throws -> Data {
        let symmetric = SymmetricKey(data: key)
        let nonce = try ChaChaPoly.Nonce(data: nonce(transcript: transcript, label: label))
        let sealed = try ChaChaPoly.seal(
            Data(),
            using: symmetric,
            nonce: nonce,
            authenticating: transcript
        )
        return sealed.ciphertext + sealed.tag
    }

    static func verifyProof(
        _ proof: Data,
        key: Data,
        transcript: Data,
        label: Data
    ) throws {
        guard proof.count == 16 else { throw SecureChannelError.invalidProof }
        let symmetric = SymmetricKey(data: key)
        let nonce = try ChaChaPoly.Nonce(data: nonce(transcript: transcript, label: label))
        let box = try ChaChaPoly.SealedBox(
            nonce: nonce,
            ciphertext: Data(),
            tag: proof
        )
        do {
            _ = try ChaChaPoly.open(box, using: symmetric, authenticating: transcript)
        } catch {
            throw SecureChannelError.invalidProof
        }
    }

    static func sharedSecret(
        _ privateKey: Curve25519.KeyAgreement.PrivateKey,
        _ publicKeyData: Data
    ) throws -> Data {
        let publicKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: publicKeyData)
        let secret = try privateKey.sharedSecretFromKeyAgreement(with: publicKey)
        return secret.withUnsafeBytes { Data($0) }
    }

    static func sign(_ message: Data, with key: Curve25519.Signing.PrivateKey) throws -> Data {
        do {
            return try key.signature(for: message)
        } catch {
            throw SecureChannelError.invalidSignature
        }
    }

    static func verify(
        _ signature: Data,
        message: Data,
        publicKey: Data
    ) throws {
        guard signature.count == 64 else { throw SecureChannelError.invalidSignature }
        let key: Curve25519.Signing.PublicKey
        do {
            key = try Curve25519.Signing.PublicKey(rawRepresentation: publicKey)
        } catch {
            throw SecureChannelError.invalidSignature
        }
        guard key.isValidSignature(signature, for: message) else {
            throw SecureChannelError.invalidSignature
        }
    }

    static func randomBytes(_ count: Int) throws -> Data {
        var data = Data(count: count)
        let status = data.withUnsafeMutableBytes { buffer in
            SecRandomCopyBytes(kSecRandomDefault, count, buffer.baseAddress!)
        }
        guard status == errSecSuccess else {
            throw SecureChannelError.invalidIdentity
        }
        return data
    }
}

// MARK: - Pairing handshake

struct PairingApproval: Sendable {
    let code: String
    let hostIdentity: HostPinnedIdentity
    let channel: SecureChannel
}

final class PairingHandshake {
    private let invite: PairingInvite
    private let identity: DeviceIdentity
    private let ephemeral: Curve25519.KeyAgreement.PrivateKey
    private let clientNonce: Data
    private var approval: PairingApproval?

    init(invite: PairingInvite, identity: DeviceIdentity) throws {
        self.invite = invite
        self.identity = identity
        self.ephemeral = Curve25519.KeyAgreement.PrivateKey()
        self.clientNonce = try HandshakeCrypto.randomBytes(32)
    }

    var c1Message: Data {
        get throws {
            let prefix = HandshakeCrypto.pairingClientPrefix(
                invite: invite,
                clientEd: identity.edPublic,
                clientX: identity.xPublic,
                ephClient: ephemeral.publicKey.rawRepresentation
            )
            let signature = try HandshakeCrypto.sign(prefix, with: identity.edPrivateKey)
            return try WireJSON.encode([
                "t": "c1",
                "invite_id": invite.inviteID,
                "ed": Base64URL.encode(identity.edPublic),
                "x": Base64URL.encode(identity.xPublic),
                "eph": Base64URL.encode(ephemeral.publicKey.rawRepresentation),
                "nc": Base64URL.encode(clientNonce),
                "sig": Base64URL.encode(signature)
            ])
        }
    }

    /// Verifies S1 and derives the channel. The six-digit code is shown to the
    /// user; no trust is stored until the user confirms and C2 succeeds.
    func accept(hostMessage data: Data) throws -> PairingApproval {
        let object = try StrictJSON.object(data, allowed: ["t", "host_ed", "host_x", "eph", "ns", "sig", "proof"])
        guard (object["t"] as? String) == "s1" else {
            throw SecureChannelError.unexpectedMessage(object["t"] as? String ?? "unknown")
        }

        let hostEd = try StrictJSON.key32(object["host_ed"])
        let hostX = try StrictJSON.key32(object["host_x"])
        let ephHost = try StrictJSON.key32(object["eph"])
        let serverNonce = try StrictJSON.key32(object["ns"])
        let signature = try StrictJSON.base64(object["sig"], field: "sig")
        let proof = try StrictJSON.base64(object["proof"], field: "proof")

        guard hostEd == invite.hostEd, hostX == invite.hostX else {
            throw SecureChannelError.handshakeIdentityMismatch
        }

        let transcript = HandshakeCrypto.pairingTranscript(
            invite: invite,
            clientEd: identity.edPublic,
            clientX: identity.xPublic,
            ephHost: ephHost,
            ephClient: ephemeral.publicKey.rawRepresentation
        )
        try HandshakeCrypto.verify(signature, message: transcript, publicKey: hostEd)

        let dh1 = try HandshakeCrypto.sharedSecret(ephemeral, ephHost)
        let dh2 = try HandshakeCrypto.sharedSecret(ephemeral, hostX)
        let dh3 = try HandshakeCrypto.sharedSecret(identity.xPrivateKey, ephHost)
        let ikm = HandshakeCrypto.concat([dh1, dh2, dh3, invite.secret])

        let keys = try HandshakeCrypto.derive(ikm: ikm, transcript: transcript)
        try HandshakeCrypto.verifyProof(
            proof,
            key: keys.confirm,
            transcript: transcript,
            label: HandshakeCrypto.s1ProofLabel
        )

        let channel = SecureChannel(c2s: keys.c2s, s2c: keys.s2c)
        let approval = PairingApproval(
            code: keys.code,
            hostIdentity: try HostPinnedIdentity(
                hostID: invite.hostID,
                hostEd: hostEd,
                hostX: hostX,
                relayURL: invite.relay
            ),
            channel: channel
        )
        self.approval = approval
        self.confirmKey = keys.confirm
        self.transcript = transcript
        self.serverNonce = serverNonce
        return approval
    }

    private var confirmKey: Data?
    private var transcript: Data?
    private var serverNonce: Data?

    /// C2 proof. Call only after the user confirms the displayed code.
    var c2Message: Data {
        get throws {
            guard let confirmKey, let transcript else {
                throw SecureChannelError.malformedMessage
            }
            let proof = try HandshakeCrypto.proof(
                key: confirmKey,
                transcript: transcript,
                label: HandshakeCrypto.c2ProofLabel
            )
            return try WireJSON.encode(["t": "c2", "proof": Base64URL.encode(proof)])
        }
    }

    func channel() throws -> SecureChannel {
        guard let channel = approval?.channel else {
            throw SecureChannelError.malformedMessage
        }
        return channel
    }
}

// MARK: - Resume handshake

struct ResumeApproval: Sendable {
    let channel: SecureChannel
}

/// Device-authenticated resume after initial pairing. Uses fresh ephemerals
/// every connection, so channel keys are never persisted or reused.
final class ResumeHandshake {
    private let host: HostPinnedIdentity
    private let identity: DeviceIdentity
    private let ephemeral: Curve25519.KeyAgreement.PrivateKey
    private let clientNonce: Data
    private var confirmKey: Data?
    private var transcript: Data?
    private var channelValue: SecureChannel?

    init(host: HostPinnedIdentity, identity: DeviceIdentity) throws {
        self.host = host
        self.identity = identity
        self.ephemeral = Curve25519.KeyAgreement.PrivateKey()
        self.clientNonce = try HandshakeCrypto.randomBytes(32)
    }

    var r1Message: Data {
        get throws {
            let message = HandshakeCrypto.resumeSignatureMessage(
                hostID: host.hostID,
                deviceEd: identity.edPublic,
                deviceX: identity.xPublic,
                ephClient: ephemeral.publicKey.rawRepresentation,
                nonce: clientNonce
            )
            let signature = try HandshakeCrypto.sign(message, with: identity.edPrivateKey)
            return try WireJSON.encode([
                "t": "r1",
                "host_id": host.hostID,
                "device_ed": Base64URL.encode(identity.edPublic),
                "device_x": Base64URL.encode(identity.xPublic),
                "eph": Base64URL.encode(ephemeral.publicKey.rawRepresentation),
                "nc": Base64URL.encode(clientNonce),
                "sig": Base64URL.encode(signature)
            ])
        }
    }

    func accept(hostMessage data: Data) throws -> ResumeApproval {
        let object = try StrictJSON.object(data, allowed: ["t", "host_ed", "host_x", "eph", "ns", "sig", "proof"])
        guard (object["t"] as? String) == "r2" else {
            throw SecureChannelError.unexpectedMessage(object["t"] as? String ?? "unknown")
        }

        let hostEd = try StrictJSON.key32(object["host_ed"])
        let hostX = try StrictJSON.key32(object["host_x"])
        let ephHost = try StrictJSON.key32(object["eph"])
        let serverNonce = try StrictJSON.key32(object["ns"])
        let signature = try StrictJSON.base64(object["sig"], field: "sig")
        let proof = try StrictJSON.base64(object["proof"], field: "proof")

        guard hostEd == host.hostEd, hostX == host.hostX else {
            throw SecureChannelError.handshakeIdentityMismatch
        }

        let transcript = HandshakeCrypto.resumeTranscript(
            hostID: host.hostID,
            deviceEd: identity.edPublic,
            deviceX: identity.xPublic,
            ephHost: ephHost,
            ephClient: ephemeral.publicKey.rawRepresentation,
            nonceClient: clientNonce,
            nonceServer: serverNonce
        )
        try HandshakeCrypto.verify(signature, message: transcript, publicKey: hostEd)

        let dh1 = try HandshakeCrypto.sharedSecret(ephemeral, ephHost)
        let dh2 = try HandshakeCrypto.sharedSecret(ephemeral, hostX)
        let dh3 = try HandshakeCrypto.sharedSecret(identity.xPrivateKey, ephHost)
        let ikm = HandshakeCrypto.concat([dh1, dh2, dh3])

        let keys = try HandshakeCrypto.derive(ikm: ikm, transcript: transcript)
        try HandshakeCrypto.verifyProof(
            proof,
            key: keys.confirm,
            transcript: transcript,
            label: HandshakeCrypto.r2ProofLabel
        )

        self.confirmKey = keys.confirm
        self.transcript = transcript
        let channel = SecureChannel(c2s: keys.c2s, s2c: keys.s2c)
        self.channelValue = channel
        return ResumeApproval(channel: channel)
    }

    var r3Message: Data {
        get throws {
            guard let confirmKey, let transcript else {
                throw SecureChannelError.malformedMessage
            }
            let proof = try HandshakeCrypto.proof(
                key: confirmKey,
                transcript: transcript,
                label: HandshakeCrypto.r3ProofLabel
            )
            return try WireJSON.encode(["t": "r3", "proof": Base64URL.encode(proof)])
        }
    }

    func channel() throws -> SecureChannel {
        guard let channelValue else {
            throw SecureChannelError.malformedMessage
        }
        return channelValue
    }
}

// MARK: - Replay window

/// Bounded replay protection for one inbound stream. Mirrors the Go contract:
/// counters start at 1, duplicates and stale counters are rejected, and a jump
/// larger than the window is refused rather than silently accepted.
struct ReplayWindow {
    static let size: UInt64 = 64

    private var highest: UInt64 = 0
    private var seen: Set<UInt64> = []

    mutating func accept(_ counter: UInt64) throws {
        guard counter > 0 else { throw SecureChannelError.invalidEnvelope }
        if highest == 0 {
            highest = counter
            seen = [counter]
            return
        }
        if counter > highest {
            guard counter - highest <= Self.size else {
                throw SecureChannelError.envelopeGap
            }
            highest = counter
            seen.insert(counter)
            prune()
            return
        }
        guard highest - counter < Self.size else {
            throw SecureChannelError.envelopeGap
        }
        guard !seen.contains(counter) else {
            throw SecureChannelError.envelopeReplay
        }
        seen.insert(counter)
    }

    private mutating func prune() {
        let floor = highest >= Self.size ? highest - Self.size : 0
        seen = seen.filter { $0 > floor }
    }
}

// MARK: - Envelope

/// Encrypts and decrypts application-plane envelopes. Envelope layout:
/// version(1) || stream(4 BE) || counter(8 BE) || ciphertext || tag, with aad
/// equal to the 13-byte header and nonce = counter || stream.
final class SecureChannel: @unchecked Sendable {
    static let maximumPlaintext = 1 << 20
    static let headerSize = 13
    static let tagSize = 16
    static let version: UInt8 = 1

    private let c2s: SymmetricKey
    private let s2c: SymmetricKey
    private let lock = NSLock()
    private var sendCounters: [UInt32: UInt64] = [:]
    private var receiveWindows: [UInt32: ReplayWindow] = [:]

    init(c2s: Data, s2c: Data) {
        self.c2s = SymmetricKey(data: c2s)
        self.s2c = SymmetricKey(data: s2c)
    }

    func seal(stream: UInt32 = 0, plaintext: Data) throws -> Data {
        guard plaintext.count <= Self.maximumPlaintext else {
            throw SecureChannelError.envelopeTooLarge
        }
        lock.lock()
        defer { lock.unlock() }

        let current = sendCounters[stream] ?? 0
        guard current < UInt64.max else { throw SecureChannelError.counterExhausted }
        let counter = current + 1
        sendCounters[stream] = counter

        var header = Data()
        header.append(Self.version)
        header.append(contentsOf: HandshakeCrypto.bigEndian(UInt32(stream)))
        header.append(contentsOf: HandshakeCrypto.bigEndian(counter))

        let nonce = try ChaChaPoly.Nonce(data: Data(header.suffix(8)) + Data(header[1..<5]))
        let sealed = try ChaChaPoly.seal(plaintext, using: c2s, nonce: nonce, authenticating: header)
        var envelope = header
        envelope.append(sealed.ciphertext)
        envelope.append(sealed.tag)
        return envelope
    }

    func open(_ envelope: Data) throws -> (stream: UInt32, plaintext: Data) {
        guard envelope.count >= Self.headerSize + Self.tagSize else {
            throw SecureChannelError.invalidEnvelope
        }
        guard envelope[envelope.startIndex] == Self.version else {
            throw SecureChannelError.envelopeVersion(Int(envelope[envelope.startIndex]))
        }
        let header = envelope.prefix(Self.headerSize)
        let stream = header.dropFirst(1).prefix(4).reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        let counter = header.dropFirst(5).prefix(8).reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        guard counter > 0 else { throw SecureChannelError.invalidEnvelope }

        let ciphertext = envelope.dropFirst(Self.headerSize).dropLast(Self.tagSize)
        let tag = envelope.suffix(Self.tagSize)
        guard ciphertext.count <= Self.maximumPlaintext else {
            throw SecureChannelError.envelopeTooLarge
        }

        let nonce = try ChaChaPoly.Nonce(data: HandshakeCrypto.bigEndian(counter) + HandshakeCrypto.bigEndian(UInt32(stream)))
        let box = try ChaChaPoly.SealedBox(nonce: nonce, ciphertext: Data(ciphertext), tag: Data(tag))
        let plaintext: Data
        do {
            plaintext = try ChaChaPoly.open(box, using: s2c, authenticating: header)
        } catch {
            throw SecureChannelError.envelopeAuthentication
        }

        lock.lock()
        defer { lock.unlock() }
        var window = receiveWindows[stream] ?? ReplayWindow()
        try window.accept(counter)
        receiveWindows[stream] = window
        return (stream, plaintext)
    }
}

// MARK: - Handshake JSON helpers

enum WireJSON {
    static func encode(_ object: [String: String]) throws -> Data {
        try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    }
}
