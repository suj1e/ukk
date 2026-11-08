# Goal: v1 product completion (unsigned distribution)

Status: active · 2026-11-08. This is the completion contract for ukk v1, not a feature wish list. A milestone is complete only when its acceptance evidence is recorded; source presence is not completion. The ordered delivery plan and per-stage evidence gates live in [mygo-implementation](mygo-implementation.md).

## Definition of done

### 1. Provider-neutral Agent runtime

- One stable bridge/Desktop contract for Agent sessions, turns, tools, approvals, capabilities, errors and bounded timelines.
- The first real provider is **omp**, using the same neutral contract as future providers.
- Codex and Claude Code remain adapter targets, not forks of the bridge or Desktop. Their contracts may be fixture-tested before a real provider run; no fake adapter is acceptable.
- Closing the Desktop, a Pane, a Tab or a Session never stops an Agent or terminal. Only explicit user intent with the agent-control capability may stop execution.
- Provider RPC child lifecycle/resume is distinct from herdr-core terminal ownership. Do not promise crash recovery merely because windows are independent.

### 2. Local Agent workspace

- Start, prompt, steer, abort, stop, observe and approve from the Desktop with explicit user intent and the separate agent-control capability.
- Terminal: real interaction (input, paste, IME, resize, named keys) over herdr-core, rendered by MyGo's native terminal surface. Observers never write or resize. Human and automatic input are source-tagged, never byte-sniffed; backpressure is bounded.
- Files/Diff/Activity and conversation panes show real data; no synthetic chat. Long Markdown renders incrementally and bounded.
- Space/Session/AgentRun management is usable without the CLI.
- Never derive tool calls, plans or approvals from ANSI terminal text.

### 3. Remote companion

- Self-hostable Go relay with WSS/TLS, operator authentication, opaque end-to-end frames, bounded routing and no raw video.
- Device identity independent of transport: short-lived pairing challenge, proof of possession on every connection, durable revocation.
- Separate pairing, agent-control, screen-view and screen-control grants.
- Native iOS/iPadOS app with real QR/camera pairing, bridge connection, chat, approvals, APNs and on-device camera.
- Remote Screen inside a Session: separate view/control grants, real media over independent STUN/TURN, explicit consent, secure stop, floating preview and temporary expansion.
- Windows/Linux capture adapters implemented where verified, or an explicitly unsupported state in the UI and docs.

### 4. Extensibility and safety

- Third-party plugin trust boundary, OS sandbox where available, durable grants and audit; capture/input permissions and revocation.
- Plugin code does not run in the bridge's privileged process by default.
- Threat review covers local IPC, relay, pairing, plugins, screen and terminal input. Denial, replay and revocation tests are required for every new mutation surface.
- This is the safety boundary required by ukk's own product, not a general-purpose plugin marketplace.

### 5. Unsigned distribution artifacts

Final release candidates are **unsigned** and clearly labelled:

| Platform | Artifact | Contents |
| --- | --- | --- |
| macOS | `.dmg` | unsigned `ukk.app` (ad-hoc signing allowed), `ukk-bridge`, README/licence, install notes |
| Windows | NSIS `.exe` | Desktop, bridge, Start Menu entry, uninstaller |
| Linux | `.deb` plus tarball | Desktop, bridge, `.desktop` entry, uninstall metadata |
| iOS | `.ipa` | device build of `UkkCompanion.app`, no signing |

An unsigned IPA is an artifact, not an installable App Store/TestFlight release; it requires later signing and a provisioning/device path. Signing, notarization and App Store distribution are explicitly out of scope.

### 6. Acceptance

- macOS, Windows and Linux: real window, keyboard/a11y smoke, Agent survive Desktop close, bridge restart/reconnect, install/uninstall.
- Physical iPhone: pairing, reconnect, approval, screen preview/control — only when signing/device prerequisites exist; otherwise recorded as **BLOCKED**, never fabricated.
- Every unsupported combination is named in [status](../status.md) and [roadmap](../roadmap.md).
- Artifacts and evidence identify the exact commit SHA and OS.

## Reset and migration

This is a greenfield implementation on native Go/MyGo Desktop, external herdr-core v0.1.18, Go bridge/relay and SwiftUI iOS. There is **no data migration**, schema compatibility or protocol compatibility with the earlier Rust/GPUI prototype. Its `main` history is historical reference only. The old repository artifacts are removed rather than kept as a parallel implementation.

## Non-goals

- Android, Web and TUI applications.
- Signing, notarization, App Store/TestFlight distribution.
- Rewriting herdr-core or forking herdr-app.
- Migrating users, SQLite data, pairings or release artifacts from the superseded Rust prototype.
- A generic plugin marketplace or public SDK beyond ukk's own safety and extension needs.
