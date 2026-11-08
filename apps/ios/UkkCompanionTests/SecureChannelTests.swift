import CryptoKit
import XCTest
@testable import UkkCompanion

final class SecureChannelTests: XCTestCase {
    // RFC 7748 §6.1 X25519 test vector.
    private let alicePrivate = Data(hex: "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
    private let alicePublic = Data(hex: "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
    private let bobPrivate = Data(hex: "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb")
    private let bobPublic = Data(hex: "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")
    private let shared = Data(hex: "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")

    // MARK: - Deterministic transcript and derivation vectors

    func testPairingTranscriptAndKeysAreDeterministic() throws {
        let invite = makeInvite(hostEd: Data(repeating: 0x11, count: 32), hostX: bobPublic)
        let clientEd = Data(repeating: 0x21, count: 32)

        let prefix = HandshakeCrypto.pairingClientPrefix(
            invite: invite,
            clientEd: clientEd,
            clientX: alicePublic,
            ephClient: alicePublic
        )
        XCTAssertEqual(prefix.hex, "1922584df9f027068b3d535d1356587111280e4f1cf25d1d1c678871a5a21cdd")

        let transcript = HandshakeCrypto.pairingTranscript(
            invite: invite,
            clientEd: clientEd,
            clientX: alicePublic,
            ephHost: bobPublic,
            ephClient: alicePublic
        )
        XCTAssertEqual(transcript.hex, "deddd4fb405a6bec2c28faa9b5e73335f1a6e7215c3c3850f74e163aea70f260")

        let ikm = HandshakeCrypto.concat([shared, shared, shared, invite.secret])
        let keys = try HandshakeCrypto.derive(ikm: ikm, transcript: transcript)
        XCTAssertEqual(keys.confirm.hex, "157d587f4816b236d85d40e5869542d9e4e15c14216e102ad7aa53862bce8d98")
        XCTAssertEqual(keys.c2s.hex, "fe8caeb536890316251c7af8db564d68a6f59d226ccd5d86368bee368fb692b2")
        XCTAssertEqual(keys.s2c.hex, "7b4de54a1685f43d68240651a842fad19db25500737d23d32be6b99216a67581")
        XCTAssertEqual(keys.code, "847903")

        // Same input must always produce the same output.
        let again = try HandshakeCrypto.derive(ikm: ikm, transcript: transcript)
        XCTAssertEqual(keys.confirm, again.confirm)
        XCTAssertEqual(keys.code, again.code)
    }

    func testResumeTranscriptAndKeysAreDeterministic() throws {
        let clientEd = Data(repeating: 0x21, count: 32)
        let clientNonce = Data(repeating: 0x31, count: 32)
        let serverNonce = Data(repeating: 0x32, count: 32)

        let signatureMessage = HandshakeCrypto.resumeSignatureMessage(
            hostID: "11111111-2222-4333-8444-555555555555",
            deviceEd: clientEd,
            deviceX: alicePublic,
            ephClient: alicePublic,
            nonce: clientNonce
        )
        XCTAssertEqual(signatureMessage.hex, "25504db1df60328d010d934cb9d5f5f1d423374c131fbfc78e9681338d4b1736")

        let transcript = HandshakeCrypto.resumeTranscript(
            hostID: "11111111-2222-4333-8444-555555555555",
            deviceEd: clientEd,
            deviceX: alicePublic,
            ephHost: bobPublic,
            ephClient: alicePublic,
            nonceClient: clientNonce,
            nonceServer: serverNonce
        )
        XCTAssertEqual(transcript.hex, "a8f393496f074811ffab533e4cee83c5e2df44be973d4237f5c3f7ab338ae2ca")

        let keys = try HandshakeCrypto.derive(ikm: HandshakeCrypto.concat([shared, shared, shared]), transcript: transcript)
        XCTAssertEqual(keys.confirm.hex, "c0490811f358bdd9564b170eccca5d3b0c56e3ba57992c78c0a350c5060c61dc")
        XCTAssertEqual(keys.c2s.hex, "a9d73fc7df0328686ec33a890d1c46488ba5e47f9cb1cc6c974be84e2398231b")
        XCTAssertEqual(keys.s2c.hex, "81fdb5915ffea776c1985e8f4f40635a5552b903838127dbd29c0fc3fe1f56fd")
        XCTAssertEqual(keys.code, "480672")
    }

    // MARK: - Envelope

    func testEnvelopeStartsWithCounterOneAndRoundTrips() throws {
        let channel = SecureChannel(c2s: Data(repeating: 1, count: 32), s2c: Data(repeating: 2, count: 32))
        let peer = SecureChannel(c2s: Data(repeating: 2, count: 32), s2c: Data(repeating: 1, count: 32))

        let envelope = try channel.seal(plaintext: Data("hello".utf8))
        XCTAssertEqual(envelope[envelope.startIndex], 1)
        XCTAssertEqual(envelope.count, SecureChannel.headerSize + 5 + SecureChannel.tagSize)
        XCTAssertEqual(decodeCounter(envelope), 1)
        XCTAssertEqual(decodeStream(envelope), 0)

        let opened = try peer.open(envelope)
        XCTAssertEqual(opened.stream, 0)
        XCTAssertEqual(opened.plaintext, Data("hello".utf8))
    }

    func testReplayedEnvelopeIsRejected() throws {
        let channel = SecureChannel(c2s: Data(repeating: 1, count: 32), s2c: Data(repeating: 2, count: 32))
        let peer = SecureChannel(c2s: Data(repeating: 2, count: 32), s2c: Data(repeating: 1, count: 32))
        let envelope = try channel.seal(plaintext: Data("once".utf8))

        _ = try peer.open(envelope)
        XCTAssertThrowsError(try peer.open(envelope)) { error in
            XCTAssertEqual(error as? SecureChannelError, .envelopeReplay)
        }
    }

    func testOutOfOrderWithinWindowIsAcceptedButStaleIsRejected() throws {
        let channel = SecureChannel(c2s: Data(repeating: 1, count: 32), s2c: Data(repeating: 2, count: 32))
        let peer = SecureChannel(c2s: Data(repeating: 2, count: 32), s2c: Data(repeating: 1, count: 32))
        let first = try channel.seal(plaintext: Data("1".utf8))
        let second = try channel.seal(plaintext: Data("2".utf8))
        let third = try channel.seal(plaintext: Data("3".utf8))

        XCTAssertEqual(try peer.open(third).plaintext, Data("3".utf8))
        XCTAssertEqual(try peer.open(first).plaintext, Data("1".utf8))
        XCTAssertEqual(try peer.open(second).plaintext, Data("2".utf8))
    }

    func testReplayWindowRejectsGapsBeyondWindow() throws {
        let channel = SecureChannel(c2s: Data(repeating: 1, count: 32), s2c: Data(repeating: 2, count: 32))
        let peer = SecureChannel(c2s: Data(repeating: 2, count: 32), s2c: Data(repeating: 1, count: 32))
        var envelopes: [Data] = []
        for index in 1...Int(ReplayWindow.size) + 2 {
            envelopes.append(try channel.seal(plaintext: Data("\(index)".utf8)))
        }
        _ = try peer.open(envelopes[0])
        XCTAssertThrowsError(try peer.open(envelopes.last!)) { error in
            XCTAssertEqual(error as? SecureChannelError, .envelopeGap)
        }
    }

    func testTamperedEnvelopeFailsAuthenticationWithoutAdvancingWindow() throws {
        let channel = SecureChannel(c2s: Data(repeating: 1, count: 32), s2c: Data(repeating: 2, count: 32))
        let peer = SecureChannel(c2s: Data(repeating: 2, count: 32), s2c: Data(repeating: 1, count: 32))
        var envelope = try channel.seal(plaintext: Data("secret".utf8))
        envelope[envelope.count - 1] ^= 0xFF

        XCTAssertThrowsError(try peer.open(envelope)) { error in
            XCTAssertEqual(error as? SecureChannelError, .envelopeAuthentication)
        }
        // A valid retry with the same counter must still succeed because the
        // failed open never consumed the counter.
        let valid = try channel.seal(plaintext: Data("retry".utf8))
        XCTAssertEqual(try peer.open(valid).plaintext, Data("retry".utf8))
    }

    func testOversizedAndWrongVersionEnvelopesAreRejected() throws {
        let channel = SecureChannel(c2s: Data(repeating: 1, count: 32), s2c: Data(repeating: 2, count: 32))
        XCTAssertThrowsError(try channel.seal(plaintext: Data(count: SecureChannel.maximumPlaintext + 1))) { error in
            XCTAssertEqual(error as? SecureChannelError, .envelopeTooLarge)
        }

        let peer = SecureChannel(c2s: Data(repeating: 2, count: 32), s2c: Data(repeating: 1, count: 32))
        var envelope = try channel.seal(plaintext: Data("x".utf8))
        envelope[envelope.startIndex] = 2
        XCTAssertThrowsError(try peer.open(envelope)) { error in
            XCTAssertEqual(error as? SecureChannelError, .envelopeVersion(2))
        }
    }

    // MARK: - Pairing handshake

    func testPairingHandshakeDerivesCodeAndChannel() throws {
        let hostSigning = Curve25519.Signing.PrivateKey()
        let hostStatic = Curve25519.KeyAgreement.PrivateKey()
        let invite = makeInvite(
            hostEd: hostSigning.publicKey.rawRepresentation,
            hostX: hostStatic.publicKey.rawRepresentation
        )
        let device = try DeviceIdentity.generate()
        let handshake = try PairingHandshake(invite: invite, identity: device)

        let c1Data = try handshake.c1Message
        let c1 = try StrictJSON.object(c1Data, allowed: ["t", "invite_id", "ed", "x", "eph", "nc", "sig"])
        XCTAssertEqual(c1["t"] as? String, "c1")
        XCTAssertEqual(c1["invite_id"] as? String, invite.inviteID)

        let clientEd = try XCTUnwrap(Base64URL.decode(c1["ed"] as? String ?? ""))
        let clientX = try XCTUnwrap(Base64URL.decode(c1["x"] as? String ?? ""))
        let clientEph = try XCTUnwrap(Base64URL.decode(c1["eph"] as? String ?? ""))
        let signature = try XCTUnwrap(Base64URL.decode(c1["sig"] as? String ?? ""))
        XCTAssertEqual(clientEd, device.edPublic)

        let prefix = HandshakeCrypto.pairingClientPrefix(
            invite: invite,
            clientEd: clientEd,
            clientX: clientX,
            ephClient: clientEph
        )
        try HandshakeCrypto.verify(signature, message: prefix, publicKey: device.edPublic)

        let ephHost = Curve25519.KeyAgreement.PrivateKey()
        let ns = try HandshakeCrypto.randomBytes(32)
        let transcript = HandshakeCrypto.pairingTranscript(
            invite: invite,
            clientEd: clientEd,
            clientX: clientX,
            ephHost: ephHost.publicKey.rawRepresentation,
            ephClient: clientEph
        )
        let clientEphKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: clientEph)
        let clientXKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: clientX)
        let dh1 = try ephHost.sharedSecretFromKeyAgreement(with: clientEphKey).withUnsafeBytes { Data($0) }
        let dh2 = try hostStatic.sharedSecretFromKeyAgreement(with: clientEphKey).withUnsafeBytes { Data($0) }
        let dh3 = try ephHost.sharedSecretFromKeyAgreement(with: clientXKey).withUnsafeBytes { Data($0) }
        let ikm = HandshakeCrypto.concat([dh1, dh2, dh3, invite.secret])
        let keys = try HandshakeCrypto.derive(ikm: ikm, transcript: transcript)

        let proof = try HandshakeCrypto.proof(
            key: keys.confirm,
            transcript: transcript,
            label: HandshakeCrypto.s1ProofLabel
        )
        let hostSignature = try hostSigning.signature(for: transcript)
        let s1 = try WireJSON.encode([
            "t": "s1",
            "host_ed": Base64URL.encode(invite.hostEd),
            "host_x": Base64URL.encode(invite.hostX),
            "eph": Base64URL.encode(ephHost.publicKey.rawRepresentation),
            "ns": Base64URL.encode(ns),
            "sig": Base64URL.encode(hostSignature),
            "proof": Base64URL.encode(proof)
        ])

        let approval = try handshake.accept(hostMessage: s1)
        XCTAssertEqual(approval.code, keys.code)

        let c2Data = try handshake.c2Message
        let c2 = try StrictJSON.object(c2Data, allowed: ["t", "proof"])
        let c2Proof = try XCTUnwrap(Base64URL.decode(c2["proof"] as? String ?? ""))
        try HandshakeCrypto.verifyProof(
            c2Proof,
            key: keys.confirm,
            transcript: transcript,
            label: HandshakeCrypto.c2ProofLabel
        )

        // The client's c2s key must match what the Host derives as its s2c key.
        let hostChannel = SecureChannel(c2s: keys.s2c, s2c: keys.c2s)
        let envelope = try approval.channel.seal(plaintext: Data("after-pairing".utf8))
        XCTAssertEqual(try hostChannel.open(envelope).plaintext, Data("after-pairing".utf8))
    }

    func testPairingRejectsHostIdentityMismatch() throws {
        let hostSigning = Curve25519.Signing.PrivateKey()
        let hostStatic = Curve25519.KeyAgreement.PrivateKey()
        let invite = makeInvite(
            hostEd: hostSigning.publicKey.rawRepresentation,
            hostX: hostStatic.publicKey.rawRepresentation
        )
        let device = try DeviceIdentity.generate()
        let handshake = try PairingHandshake(invite: invite, identity: device)

        let transcript = HandshakeCrypto.pairingTranscript(
            invite: invite,
            clientEd: device.edPublic,
            clientX: device.xPublic,
            ephHost: hostStatic.publicKey.rawRepresentation,
            ephClient: device.xPublic
        )
        let signature = try hostSigning.signature(for: transcript)
        let s1 = try WireJSON.encode([
            "t": "s1",
            "host_ed": Base64URL.encode(Data(repeating: 9, count: 32)),
            "host_x": Base64URL.encode(hostStatic.publicKey.rawRepresentation),
            "eph": Base64URL.encode(hostStatic.publicKey.rawRepresentation),
            "ns": Base64URL.encode(Data(repeating: 1, count: 32)),
            "sig": Base64URL.encode(signature),
            "proof": Base64URL.encode(Data(repeating: 2, count: 16))
        ])

        XCTAssertThrowsError(try handshake.accept(hostMessage: s1)) { error in
            XCTAssertEqual(error as? SecureChannelError, .handshakeIdentityMismatch)
        }
    }

    // MARK: - Resume handshake

    func testResumeHandshakeSucceedsWithPinnedHost() throws {
        let hostSigning = Curve25519.Signing.PrivateKey()
        let hostStatic = Curve25519.KeyAgreement.PrivateKey()
        let host = try HostPinnedIdentity(
            hostID: "11111111-2222-4333-8444-555555555555",
            hostEd: hostSigning.publicKey.rawRepresentation,
            hostX: hostStatic.publicKey.rawRepresentation,
            relayURL: URL(string: "wss://relay.example.invalid/ws")!
        )
        let device = try DeviceIdentity.generate()
        let handshake = try ResumeHandshake(host: host, identity: device)

        let r1Data = try handshake.r1Message
        let r1 = try StrictJSON.object(r1Data, allowed: ["t", "host_id", "device_ed", "device_x", "eph", "nc", "sig"])
        let clientEph = try XCTUnwrap(Base64URL.decode(r1["eph"] as? String ?? ""))
        let nc = try XCTUnwrap(Base64URL.decode(r1["nc"] as? String ?? ""))
        let signature = try XCTUnwrap(Base64URL.decode(r1["sig"] as? String ?? ""))
        let message = HandshakeCrypto.resumeSignatureMessage(
            hostID: host.hostID,
            deviceEd: device.edPublic,
            deviceX: device.xPublic,
            ephClient: clientEph,
            nonce: nc
        )
        try HandshakeCrypto.verify(signature, message: message, publicKey: device.edPublic)

        let ephHost = Curve25519.KeyAgreement.PrivateKey()
        let ns = try HandshakeCrypto.randomBytes(32)
        let transcript = HandshakeCrypto.resumeTranscript(
            hostID: host.hostID,
            deviceEd: device.edPublic,
            deviceX: device.xPublic,
            ephHost: ephHost.publicKey.rawRepresentation,
            ephClient: clientEph,
            nonceClient: nc,
            nonceServer: ns
        )
        let clientEphKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: clientEph)
        let deviceXKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: device.xPublic)
        let dh1 = try ephHost.sharedSecretFromKeyAgreement(with: clientEphKey).withUnsafeBytes { Data($0) }
        let dh2 = try hostStatic.sharedSecretFromKeyAgreement(with: clientEphKey).withUnsafeBytes { Data($0) }
        let dh3 = try ephHost.sharedSecretFromKeyAgreement(with: deviceXKey).withUnsafeBytes { Data($0) }
        let keys = try HandshakeCrypto.derive(
            ikm: HandshakeCrypto.concat([dh1, dh2, dh3]),
            transcript: transcript
        )
        let proof = try HandshakeCrypto.proof(
            key: keys.confirm,
            transcript: transcript,
            label: HandshakeCrypto.r2ProofLabel
        )
        let hostSignature = try hostSigning.signature(for: transcript)
        let r2 = try WireJSON.encode([
            "t": "r2",
            "host_ed": Base64URL.encode(host.hostEd),
            "host_x": Base64URL.encode(host.hostX),
            "eph": Base64URL.encode(ephHost.publicKey.rawRepresentation),
            "ns": Base64URL.encode(ns),
            "sig": Base64URL.encode(hostSignature),
            "proof": Base64URL.encode(proof)
        ])

        let approval = try handshake.accept(hostMessage: r2)
        let r3Data = try handshake.r3Message
        let r3 = try StrictJSON.object(r3Data, allowed: ["t", "proof"])
        let r3Proof = try XCTUnwrap(Base64URL.decode(r3["proof"] as? String ?? ""))
        try HandshakeCrypto.verifyProof(
            r3Proof,
            key: keys.confirm,
            transcript: transcript,
            label: HandshakeCrypto.r3ProofLabel
        )

        let hostChannel = SecureChannel(c2s: keys.s2c, s2c: keys.c2s)
        let envelope = try approval.channel.seal(plaintext: Data("resumed".utf8))
        XCTAssertEqual(try hostChannel.open(envelope).plaintext, Data("resumed".utf8))
    }

    func testResumeRejectsDifferentPinnedHost() throws {
        let hostSigning = Curve25519.Signing.PrivateKey()
        let hostStatic = Curve25519.KeyAgreement.PrivateKey()
        let host = try HostPinnedIdentity(
            hostID: "11111111-2222-4333-8444-555555555555",
            hostEd: hostSigning.publicKey.rawRepresentation,
            hostX: hostStatic.publicKey.rawRepresentation,
            relayURL: URL(string: "wss://relay.example.invalid/ws")!
        )
        let device = try DeviceIdentity.generate()
        let handshake = try ResumeHandshake(host: host, identity: device)
        let r2 = try WireJSON.encode([
            "t": "r2",
            "host_ed": Base64URL.encode(Data(repeating: 7, count: 32)),
            "host_x": Base64URL.encode(Data(repeating: 8, count: 32)),
            "eph": Base64URL.encode(Data(repeating: 8, count: 32)),
            "ns": Base64URL.encode(Data(repeating: 1, count: 32)),
            "sig": Base64URL.encode(Data(repeating: 2, count: 64)),
            "proof": Base64URL.encode(Data(repeating: 3, count: 16))
        ])
        XCTAssertThrowsError(try handshake.accept(hostMessage: r2)) { error in
            XCTAssertEqual(error as? SecureChannelError, .handshakeIdentityMismatch)
        }
    }

    // MARK: - Helpers

    private func makeInvite(hostEd: Data, hostX: Data) -> PairingInvite {
        PairingInvite(
            version: 1,
            hostID: "11111111-2222-4333-8444-555555555555",
            hostEd: hostEd,
            hostX: hostX,
            inviteID: "invite-123456",
            secret: Data(repeating: 0x42, count: 32),
            expireMS: 1_762_550_000_000,
            relay: URL(string: "wss://relay.example.invalid/ws")!
        )
    }

    private func decodeCounter(_ envelope: Data) -> UInt64 {
        envelope.dropFirst(5).prefix(8).reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
    }

    private func decodeStream(_ envelope: Data) -> UInt32 {
        envelope.dropFirst(1).prefix(4).reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
    }
}

private extension Data {
    init(hex: String) {
        var bytes: [UInt8] = []
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            bytes.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        self.init(bytes)
    }

    var hex: String { map { String(format: "%02x", $0) }.joined() }
}
