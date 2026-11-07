# Development guide (planned)

## Status

Architecture-first documentation repository. The code layout is a proposal, not a runnable build. Do not publish imaginary setup commands as if `cargo run` already builds ukk.

## Toolchain

- Rust stable pinned by `rust-toolchain.toml` once project is initialized; cargo fmt/clippy/test.
- GPUI source version pinned to a tested git revision, with compatible component library versions.
- macOS / Xcode with code signing for future ScreenCaptureKit and Accessibility permission integration.
- Swift toolchain and Xcode project for iOS app; Swift Package Manager where appropriate.
- herdr-core `hc` installed separately and API schema exported from the pinned runtime.
- Node.js only if using self-hosted alma-relay, which remains a **candidate** transport, not a part of the Rust Host.

## Planned source layout

```text
apps/desktop/               Rust GPUI app
apps/ios/                   SwiftUI app
crates/domain/              stable IDs and data model
crates/protocol/            versioned command/event envelopes
crates/host/                background agent service
crates/hc-client/           OpenRPC and terminal framing
crates/agent-adapters/      structured provider events
crates/plugin-api/          manifest and extension contract
crates/plugin-host/         supervisor and permission broker
crates/remote/              pairing and relay transport
crates/persistence/         SQLite migrations, host metadata
plugins/remote-screen/      privileged Mac screen helper
docs/                       architecture, product, ADR and roadmap
```

## Early integration spikes

1. Install and pin herdr-core, capture output of `hc schema`; validate commands in source before coding against assumptions.
2. Implement minimal hc-client connect/list/attach/subscribe and close/restore test.
3. Prototype GPUI window + tabs/splits + an agent list without requiring terminal emulation as primary UI.
4. Build fake Host fixtures and protocol tests for iOS, then connect a real Host.
5. Test QR session, independent device trust and API authorization *before* remote mutation is allowed.
6. Build isolated sample plugin before privileged Remote Screen.
7. Verify ScreenCaptureKit + VideoToolbox + WebRTC transport, real iOS hardware decode and NAT fallback.

## Engineering conventions

- Document public API schemas and snapshot/event cursor semantics.
- Keep OS integrations behind adapters with clear unsupported-path errors.
- All direct code change/approval commands require explicit user-intent and authz.
- No plaintext secrets in repo, logs, screenshots, issue descriptions or push payloads.
- Every integration claiming support needs a test and named upstream version.
- Use ADRs for decisions; keep compatibility matrix updated.
- No silent process destruction when a view closes. Distinguish `detach`, `archive`, `stop` and `kill`.

## Tests planned

Unit: command validation, entity mapping, grant checks, idempotency. Integration: hc API/event restart; plugin crash; protocol cursor gaps; multi-client read/write. E2E: Mac/iPhone pairing, revoked device, denied screen permissions, local sleep, relay disconnect and TURN fallback.

See [roadmap](ROADMAP.md).
