# ukk iOS companion

Native SwiftUI iPhone/iPad client. It is a real remote client for a paired
Desktop Host: pairing runs the v1 handshake, every Session view is backed by
encrypted RPC, and Remote Screen uses a real WebRTC peer connection.

The app starts in an explicitly labelled **fixture mode only when no Host has
been paired** (`FIXTURE DATA — no Host connection`). As soon as a Host is
pinned, fixture content is not used; cached Host data is labelled stale whenever
the connection is connecting, degraded, offline, suspended or revoked.

## What is implemented

- **Transport** (`Transport/`)
  - `RelayClient`: `URLSessionWebSocketTask` with base64 relay frames, a 20 s
    keepalive ping and typed failures.
  - `SecureChannel`: CryptoKit X25519, Ed25519, HKDF-SHA256 and
    ChaCha20-Poly1305, matching `apps/desktop/internal/remote/doc.go` byte for
    byte. Pairing C1/S1/C2, device-authenticated resume R1/R2/R3, six-digit
    confirmation code, 1 MiB envelopes, counters starting at 1 and a 64-entry
    replay window.
  - `RemoteAPI`: request/response correlation by 8-byte hex id, typed errors,
    concurrent requests, pushed `event` messages.
  - `RemoteHostClient`: fresh resume handshake per connection, bounded
    exponential backoff (1, 2, 4, 8, 16, 30 s), status stream and RPC surface.
- **Pairing** (`Screens/QRPairingScreen.swift`, `Transport/PairingCoordinator.swift`)
  - Scan or paste a v1 invite, show the six-digit code, require explicit
    confirmation, then send C2 and verify the encrypted channel with `health`
    before storing trust.
  - Invite secrets are never stored, displayed or logged. The invite is single
    use and expires within five minutes.
- **Keychain** (`Transport/Keychain.swift`)
  - Device Ed25519 seed and X25519 private key, plus the pinned Host public
    keys and relay URL, in generic-password items with
    `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` and no iCloud sync.
  - Channel keys and counters are never persisted; reconnect/relaunch always
    performs a fresh resume handshake.
- **Host-backed Sessions**
  - `workspace.get` drives Spaces and Sessions; `agent.snapshot` drives messages,
    turns, provider tool calls and pending approvals; `agent.list` is available
    for event-shaped snapshots.
  - `agent.prompt`, `agent.abort` (turn only), `agent.stop` (explicit process
    stop) and `agent.approve` with explicit confirmation.
  - Approvals are shown only when the Host granted `agent.approve` and the
    provider emitted a pending request. Terminal text is never parsed.
- **Trusted devices**
  - `remote.status` loads devices, grants and expiries; `remote.revoke` revokes.
    Revoking this device ends the connection and requires re-pairing.
- **Remote Screen** (`Screen/`)
  - LiveKitWebRTC `150.7871.05` via SPM. The Host's `screen.start` offer and
    ICE (STUN/TURN) configuration feed an `LKRTC*` peer connection; the answer
    returns through `screen.answer`; the video track renders in
    `LKRTCMTLVideoView` inside the Session tile, with expand/collapse and
    one-tap stop (`screen.stop`).
  - Pointer input is sent only when the Host's `screen.start` result reports a
    control grant, mapped into the captured frame's pixel space.
  - If no video track arrives within 12 s the tile reports an explicit
    unavailable state. There is no fake poster.
- **Push contract** (`Domain/PushHintContracts.swift`)
  - Still an opaque-hint allowlist (`ukk_hint_id`). APNs registration, delivery
    and background handling are **not** implemented.
- **Persistence**: only the pinned Host (Keychain) and the last selected
  Host-backed Session id (UserDefaults) are persisted.

## Layout

```text
apps/ios/
  project.yml                  XcodeGen specification (source of truth)
  UkkCompanion.xcodeproj       generated; regenerate with `xcodegen generate`
  UkkCompanion/
    App/                       entry point + app model
    Domain/                    models, fixtures, navigation, state machine,
                               capabilities, devices, push contract
    Transport/                 relay, crypto, RPC, Keychain, pairing, host client
    Screen/                    WebRTC controller and Remote Screen tile
    Scanner/                   AVFoundation QR scanner + availability
    Screens/                   workspace/space/session/agent/devices/pairing
    Components/                theme, badges, chat bubbles, cards
    Resources/                 asset catalogue
  UkkCompanionTests/           unit tests (no simulator UI)
  UkkCompanionUITests/         black-box fixture-mode smoke test
```

## Build

```bash
cd apps/ios
xcodegen generate
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' build
```

The LiveKit binary target is required; the build fails rather than silently
falling back to a video-less or fake screen.

## Tests

```bash
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -sdk iphonesimulator -destination 'platform=iOS Simulator,name=iPhone 17 Pro' \
  -only-testing:UkkCompanionTests test
```

Covered: invite parsing (strict fields, wss, userinfo, expiry, key lengths),
fixed-key transcript/HKDF vectors for pairing and resume, AEAD proofs, envelope
counter/replay/gap/authentication, Keychain round-trip via a test store, RPC
correlation/errors/events, connection transitions and the fixture-mode
guarantees.

## Limitations

- APNs is a contract only; no token registration or delivery exists.
- Remote Screen is view + pointer input. Keyboard and wheel helpers exist in the
  client, but the tile exposes pointer input only.
- No signing or notarization is claimed.
