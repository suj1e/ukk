# ukk — Implementation status

> **Snapshot:** 2026-11-08, reset to native Go/MyGo Desktop + external herdr-core + SwiftUI iOS. This page records **exact evidence**, not intentions. Where an item has not been verified, it says so. The integration owner appends build, CI, runtime and device evidence as it lands; planned module paths in [tech-design](tech-design.md) are not proof that code exists.

## Product contract (fixed)

- **Desktop:** native Go application built with MyGo (native UI), macOS/Windows/Linux from one codebase. No Rust/GPUI application, no webview primary UI.
- **Runtime ownership:** external herdr-core v0.1.18 owns persistent terminals, processes and structured status. ukk never copies or rewrites it. Closing a Pane/Tab/Session/window detaches and never stops an Agent.
- **Service:** `ukk-bridge` is the background authorization and durable-metadata service for local clients, remote devices, agent adapters and screen connections. `ukk-relay` is the separate self-hostable control-plane relay.
- **Domain:** Space → Session → AgentRun; Pane/Tab is a view. No mandatory Planner/Coder/Reviewer/Tester workflow.
- **Mobile:** native SwiftUI iOS/iPadOS is active scope. Android, Web and TUI remain deferred.
- **Remote Screen:** opt-in Session tile, separate view/control grants, independent STUN/TURN media, never raw video on the relay.
- **Provider truth:** omp is the first real provider through a neutral contract. Codex/Claude contracts may be fixture-tested; no fake adapters and no ANSI-derived approvals/tools.
- **Reset:** greenfield. No data, schema, protocol or artifact migration from the earlier Rust/GPUI prototype. Its `main` (including `f033cf0`) is historical reference only.

Canonical documents: [requirements](requirements.md), [tech-design](tech-design.md), [v1 completion](goals/v1-complete.md), [implementation plan](goals/mygo-implementation.md), [security](subsystems/security.md).

## Current milestone status

This table records the **claim**, not a promise. `Pending evidence` means the work/verification result has not been recorded here yet; it does not mean the feature is permanently absent.

| Stage | Claim | Evidence boundary |
| --- | --- | --- |
| Docs / architecture reset | Active docs describe the native Go/MyGo + external herdr-core + SwiftUI structure; obsolete Rust/GPUI claims removed | Documentation change only |
| M0 Cutover + build spine | **Local macOS:** module builds, `ukk.app` packages ad-hoc (20.6 MB) with the pinned `libghostty-vt.dylib`, and the packaged app opens a native window and self-starts the bridge | `gofmt -l .` empty, `go vet ./...` clean, `go test ./...` 293 tests/18 packages (2026-11-08); no CI run yet, no Windows/Linux runtime check |
| M1 Local workspace + terminals | **Local macOS:** real herdr-core v0.1.18 terminal `t1z` attached read/write and rendered in the MyGo terminal; observe/credit/resize behavior has package tests | Window-close process survival, observer denial against a real core, and Windows/Linux attach remain unverified |
| M2 Provider-neutral omp | **Local macOS:** bridge-owned omp 18.18.1 session started, accepted a prompt, streamed a completed turn and usage, and rendered in the Desktop conversation (`OK`, 13,622 tokens) | Long-Markdown bounds, approvals against a provider that emits them, and provider restart/resume remain unverified |
| M3 Relay + pairing + iOS | Go pairing, resume, envelope, grants and relay routing have package tests; iOS transport is being implemented | No deployed WSS relay, no WAN proof, no physical-device pairing/camera/APNs |
| M4 Remote Screen | Capture, Annex-B, pion publisher, input gating and bridge session management have package tests | No real TURN media session, permission/stop evidence, or iPhone viewer |
| M5 Plugin safety | Manifest/grants/supervisor/framing have 93 package tests; bridge API and a Settings surface are wired | No malicious-plugin sandbox run; helpers are process-isolated, not OS-sandboxed |
| M6 Unsigned distribution | A local macOS `.app` was built by `go tool mygo build` | No dmg/NSIS/deb artifacts from CI, no install matrix, no IPA |

Do not copy a green result from one milestone or SHA to another. Do not treat an existing `apps/ios` fixture screen as Host integration.

## What is retained from before the reset

- **iOS fixture shell:** `apps/ios` native SwiftUI navigation, fixture-labelled chat, trust shell, QR scanner code and reserved screen slot may be retained as UI scaffolding. It is not evidence of a real bridge, relay, pairing, approvals, APNs or camera session.
- **Historical Rust implementation:** the previous `main` and its merged PRs are historical context only. They are not a compatibility target, are not migrated, and must not be cited as current source. Old crate/module paths and commands do not describe this tree.
- **Retained documentation:** product/architecture docs, goals, decisions, references, the domain/security/plugins subsystem docs, release/incident runbooks and release assets remain the active documentation set.

## Verification boundaries

- **Source code:** a file, package or entrypoint existing does not mean the feature works.
- **CI:** only a green workflow run on the exact SHA counts. A configured workflow or local command that was not observed is not evidence.
- **Native GUI:** window creation, keyboard/accessibility, permission dialogs and service installation require OS-specific real-window evidence.
- **herdr-core:** requires a pinned real daemon plus compatibility tests; mock transports alone are not process-survival evidence.
- **omp:** requires a real provider run through the bridge; CLI output pasted as a UI claim is not GUI evidence.
- **Relay/TURN:** requires a deployed WSS/TLS relay, operator-auth denial, E2E tamper/replay checks, revocation and actual media over STUN/TURN. A local mock relay is not WAN evidence.
- **iOS:** simulator build/tests do not prove camera, APNs or physical-device pairing. Signing/device prerequisites must exist before claiming PASS.
- **Artifacts:** checksums and `--version` identity must match the exact SHA; an unsigned IPA is non-installable.

## Safety invariants under test

- No Pane/Tab/Session/window close sends stop/kill; only explicit user intent with agent-control may stop execution.
- Human vs automatic input is source-tagged on the single write path; never byte-sniffed.
- Observers never write or resize; controller leases are explicit and expiring.
- RESIZE is applied before paint; backpressure is bounded.
- No ANSI-to-approval/tool inference.
- Pairing, agent control, screen view and screen control are separate grants.
- Relay carries control frames only; screen media uses independent STUN/TURN.
- Third-party plugin code does not run in the privileged bridge process by default.
- Screen capture/input has visible consent, a one-tap stop and revoke teardown.

## Open acceptance gaps

The open work is the ordered plan in [mygo-implementation](goals/mygo-implementation.md), not a separate wish list:

1. M0: active-tree cutover, single Go module and real native-shell build evidence.
2. M1: real herdr-core process survival, attach/detach, provenance, resize and backpressure.
3. M2: real omp UI run, tools/approvals where supported, bounded long Markdown, future-provider fixtures.
4. M3: deployed relay/TLS/operator auth, E2E possession/revocation, WAN reconnect, iOS simulator tests; physical camera/APNs gate deferred.
5. M4: real STUN/TURN media, capture/input permissions, stop/lease/revocation, iOS overlay.
6. M5: sandboxed third-party plugins, durable grants/audit, denial/replay review.
7. M6: unsigned installers/artifacts, three-OS install/window/a11y matrix, bridge restart, physical-iPhone gate where prerequisites exist.

## Maintenance

One integration owner updates this page after merges and acceptance runs. Record exact SHA, OS/device, command or workflow URL, artifact hash and observed result. Mark **NOT VERIFIED** or **BLOCKED** rather than inferring success. Milestone stages are implementation order, not user-confirmation gates.
