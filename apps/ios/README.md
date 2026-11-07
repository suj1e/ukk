# ukk iOS companion (fixture-first shell)

Roadmap task **I1**: a native SwiftUI iPhone/iPad shell. This app is deliberately
**fixture-first**: it renders deterministic sample content and never contacts a
Host. Real remote sessions, trust establishment and approvals belong to the
S1/S2/R1 lanes and are not implemented here.

This directory contains **no WebView**, no Android target, no GPUI fork and no
root Rust workspace changes.

## What is implemented

- **Navigation**: Workspace → Space → Session → AgentRun, backed by a typed,
  unit-tested `NavigationState` and `NavigationStack` routes.
- **Chat-first Session screen**: user/agent/system messages, agent chips,
  per-message fixture metadata. The composer is present but disabled.
- **Connection indicators**: `ConnectionStateMachine` models
  fixture/online/reconnecting/offline/suspended/revoked with bounded deterministic
  backoff. The app starts in `fixtureData` and never claims "Live".
- **Backgrounding contract**: `scenePhase` background suspends the transport
  state; foreground requires a fresh authenticated handshake before going live.
- **Approvals**: explicitly unsupported. `ApprovalPanelState.actions` is always
  empty; the UI contains no approve/reject affordance. Terminal text is never
  parsed into approvals.
- **Trusted devices**: local management shell with roles, per-capability scopes
  and local-only revocation. The current device cannot revoke itself.
- **QR pairing presentation**: AVFoundation `AVCaptureMetadataOutput` QR scanner
  with camera-unavailable state (simulator), a documented-invite parser and a
  secret-redacting `description`. Parsing is local; no network, no trust storage.
- **Reserved Session overlay slot**: a non-interactive placeholder for the future
  M1 Remote Screen plugin. No capture, media or pointer path exists.
- **Push contract**: an opaque-hint allowlist (`ukk_hint_id`). APNs registration,
  delivery and background handling are **not implemented**.

Every screen shows a persistent **FIXTURE DATA — no Host connection** banner;
entity rows carry a FIXTURE badge and messages are stamped `fixture`.

## Layout

```text
apps/ios/
  project.yml                  XcodeGen specification (source of truth)
  UkkCompanion.xcodeproj       generated; regenerate with `xcodegen generate`
  UkkCompanion/
    App/                       entry point + observable app model
    Domain/                    models, fixtures, navigation, connection machine,
                               approvals/capabilities, devices, pairing, push
    Scanner/                   AVFoundation QR scanner + camera availability
    Screens/                   workspace/space/session/agent/devices/pairing
    Components/                theme, badges, chat bubbles, cards
    Resources/                 asset catalogue
  UkkCompanionTests/           unit tests (no simulator UI)
  UkkCompanionUITests/         black-box UI smoke test
```

Regenerate the project after editing `project.yml`:

```bash
cd apps/ios
xcodegen generate
```

## Build and test

```bash
cd apps/ios

# Compile for the simulator SDK (no runtime needed)
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' build

# Run unit + UI tests on an installed iPhone simulator
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=26.5' test

# Only the fast unit suite
xcodebuild -project UkkCompanion.xcodeproj -scheme UkkCompanion \
  -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=26.5' \
  -only-testing:UkkCompanionTests test
```

If `xcrun simctl` or `xcodebuild` reports
`CoreSimulator is out of date. Current version (...) is older than build version (...)`,
the selected Xcode and the machine-wide CoreSimulator install are mismatched. The
supported fix is to install the matching additional components for the installed
Xcode (opening Xcode's "Install additional components" prompt, or
`xcodebuild -runFirstLaunch`). Merely switching `DEVELOPER_DIR` to an older Xcode
can also fail on a newer macOS: that Xcode's `DTServiceHub`/`dtarbiter` helpers may
be rejected by AMFI (`com.apple.private.amfi.version-restriction`). Record which
Xcode, simulator runtime and helper versions were actually used; compiling with a
newer SDK does not prove test execution.

## Test coverage

`UkkCompanionTests`:

- navigation push/depth/selection, duplicate push, workspace replacement, reset;
- connection state machine: fixture start, live only after transport open,
  reconnect backoff and terminal offline, background → foreground handshake,
  revocation terminality, indicator honesty, bounded retry delay;
- fixture integrity: deterministic IDs, every entity marked fixture, relationship
  resolution, ordered messages, all capabilities unnegotiated, unknown-status
  provenance;
- approvals: unsupported reasons, empty actions, read-only composer, reserved
  overlay slot;
- trusted devices: ordering, revocation outcomes, least-privilege scopes;
- pairing invite parsing: valid payload, expiry, version, relay scheme, UUID,
  key charset, secret length, secret redaction;
- push hint allowlist and sanitization;
- simulator camera availability.

`UkkCompanionUITests` (black box): fixture banner, non-live status, full
navigation to the Session chat, unsupported approval state, disabled composer,
reserved Remote Screen slot, agent drill-down, trusted-device shell, local
revocation and the simulator camera-unavailable pairing screen.

## Validation record (2026-11-08)

- Generic simulator build with **Xcode 27.2** (iOS 27.2 SDK) succeeded. The host's
  CoreSimulator mismatch warning appears during resolution but does not affect
  compilation.
- The full test scheme ran on an **iPhone 16 simulator, iOS 26.0 (23A343)**:
  **58 unit tests + 3 UI tests passed, 0 failures**. Xcode 27.2 could not load
  simulator devices on this host because CoreSimulator 1174.21 is not installed;
  the run therefore used **Xcode 27.0 beta** (matching CoreSimulator 1174.9.2).
  That Xcode's instrument helpers were rejected by AMFI on macOS 27.2, so
  newer signed `DTServiceHub`/`dtarbiter`/`dtsecurity` helper binaries were
  temporarily substituted for the run and then restored (signatures verified).
  The supported long-term fix is installing the matching Xcode components.
- This is a single-host simulator result, not CI and not device acceptance.

## Security notes

- No credentials or trust material are stored. The pairing invite secret is held
  only in memory while validating a pasted payload, is never displayed, logged or
  persisted, and `PairingInvite.description` redacts it.
- The QR scanner stops capture after the first successful read.
- No permissions are widened implicitly: every capability flag starts false and
  every device scope is separate. Approvals are structurally unavailable.
- No network requests exist in this build; the only `NS*UsageDescription` is
  camera, used solely for QR presentation.

## NOT VERIFIED

- On-device iPhone/iPad execution, camera permission prompts and real QR scanning.
- Any real Host integration, pairing handshake, relay transport, event replay or
  command dispatch; this requires S1/S2/R1.
- APNs registration/delivery, background push and notification actions.
- Remote Screen media/control (M1); only the reserved slot exists.
- iPad multi-column/adaptivity QA beyond the shared `TARGETED_DEVICE_FAMILY`
  configuration, dynamic type extremes, VoiceOver and localization.
